use std::collections::{VecDeque, HashSet};
use swayipc::{Connection, Node, NodeLayout};
use swayipc::Error::CommandParse;
use clap::{Command, Arg};


#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Could not open a connection to sway") ]
    Conn,
    #[error("Could not get the node layout tree") ]
    GetTree,
    #[error("Could not get the workspaces") ]
    GetWorkspaces,
    #[error("Error issuing resize command") ]
    Resize,
    #[error("Error issuing swap command") ]
    Swap,
    #[error("Node disappeared while running") ]
    NodeGone,
    #[error("Current focus could not be determined") ]
    NoFocus,
}


/// Breadth first search for the first node for which `predicate` holds
fn bfsearch<'a>(root: &'a Node, predicate: impl Fn(&'a Node) -> bool)
-> Option<&'a Node> 
{
    let mut q = VecDeque::from(vec![root]);

    while let Some(n) = q.pop_front() {
        if predicate(n) { 
            return Some(n) 
        }

        q.extend(n.nodes.iter());
    };

    None // Never found
}

/// Find a node with `id` in some (sub-)tree
fn find_by_id(root: &Node, id: i64) -> Option<&Node> {
    bfsearch(root, |n| n.id == id)
}

/// Find the highest level node that is focused
fn top_focus(root: &Node) -> Option<&Node> {
    bfsearch(root, |n| n.focused)
}


fn resize_node(conn: &mut Connection, n: &Node, parent: &Node) 
-> Result<(), AppError> {
    let Node{ id: child_id, .. } = n;
    let Node{ nodes: children, .. } = parent;

    let (get_dim, dir): (fn(&Node) -> i32, &str)= match parent.layout {
        NodeLayout::SplitH => (|n| n.rect.width, "right"),
        NodeLayout::SplitV => (|n| n.rect.height, "down"),
        _ => return Ok(()),
    };

    let desired_dim = get_dim(parent) / children.len() as i32;
    let diff = get_dim(n) - desired_dim;
    assert!(diff >= 0, "Nodes are not ordered correctly");

    let cmd = format!("[con_id={child_id}] resize shrink {dir} {diff} px");
    let res = conn.run_command(cmd).map_err(|_| AppError::Resize)?;

    match res.first().ok_or(AppError::Resize)? {
        Ok(_)                   => Ok(()),
        Err(_)                  => Err(AppError::Resize),
    }
}

/// For a given node id, get its info using a new swayipc call
fn get_latest_info(conn: &mut Connection, node_id: i64) 
-> Result<Node, AppError> {
    let tree = conn.get_tree().map_err(|_| AppError::GetTree)?;
    find_by_id(&tree, node_id).ok_or(AppError::NodeGone).cloned()
}

/// Calculate the minimum amount of swaps to go from one order to another
fn min_swaps(ord1: &[i64], ord2: &[i64]) -> Vec<(i64,i64)> {
    assert!(ord1.len() == ord2.len(), 
        "inputs should be permutations of each other");

    let mut swaps: Vec<(i64,i64)> = Vec::new();
    let mut visited: Vec<bool> = vec![false; ord1.len()];
    let mut res = ord1.to_vec();

    for i in 0..ord1.len() {
        let mut idx = i;

        loop {
            if res[i] == ord2[i] { break; }
            if visited[idx] { break; }
            visited[idx] = true;

            // TODO position is O(n), should we do better?
            let target_idx = ord2.iter().position(|y| *y == res[idx]).unwrap();

            res.swap(idx, target_idx);
            swaps.push((
                *ord1.get(idx).unwrap(), 
                *ord1.get(target_idx).unwrap()));

            idx = target_idx;
        }
    }
    swaps
}

fn balance(conn: &mut Connection, root: &Node) -> Result<(), AppError> {
    let mut q: VecDeque<i64> = VecDeque::from(vec![root.id]);
    type DimGetter = fn(&Node) -> i32;


    while let Some(cur_id) = q.pop_front() {
        let cur = get_latest_info(conn, cur_id)?;
        if cur.nodes.is_empty() { continue }

        let (get_dim, get_pos): (DimGetter, DimGetter) = match cur.layout {
            NodeLayout::SplitH => (|n| n.rect.width, |n| n.rect.x),
            NodeLayout::SplitV => (|n| n.rect.height, |n| n.rect.y),
            _ => unreachable!("Handled in caller"),
        };

        // Sort based on size
        let mut sorted_nodes = cur.nodes.clone();
        sorted_nodes.sort_by(|x,y| i32::cmp(&get_dim(y), &get_dim(x)));

        // Swap the nodes to the order just found
        let initial_order: Vec<i64> = cur.nodes.iter().map(|n| n.id).collect();
        let target_order: Vec<i64> = sorted_nodes.iter().map(|n| n.id).collect();
        let swaps = min_swaps(&initial_order, &target_order);

        for id in &target_order {
            eprintln!("{id}: {}", find_by_id(root, *id).unwrap().rect.width);
        }

        eprintln!("initial_order: {initial_order:?}");
        eprintln!("target_order: {target_order:?}");
        eprintln!("swaps: {swaps:?}");
        
        for (x,y) in &swaps {
            let cmd = format!("[con_id={x}] swap container with con_id {y} px");
            eprintln!("Executing: {cmd}");
            conn.run_command(cmd)
                .map_err(|_| AppError::Swap)?
                .pop().ok_or(AppError::Swap)? 
                .map_err(|_| AppError::Swap)?;
        }

        // // Shrink nodes from left to right
        // let all_except_last = cur.nodes.iter().take(cur.nodes.len()-1);
        // for child in all_except_last {
        //     resize_node(conn, child, &cur)?;
        // }
        // 
        // // swap back to initial order
        // for (x,y) in swaps.iter().rev() {
        //     let cmd = format!("[con_id={x}] swap container with con_id {y} px");
        //     conn.run_command(cmd)
        //         .map_err(|_| AppError::Swap)?
        //         .pop().ok_or(AppError::Swap)? 
        //         .map_err(|_| AppError::Swap)?;
        // }
        // q.extend(cur.nodes.iter().map(|n| n.id));
    }

    Ok(())
}


fn main() -> Result<(),AppError> {
    let arg_matches = Command::new("sway-balance")
        .author("Rintse")
        .about("Balance a sway workspace, or some focus therein")
        .arg(Arg::new("focus")
            .long("focus")
            .short('f')
            .help("Balance the focus, instead of the entire container")
            .action(clap::ArgAction::SetTrue))
        .get_matches();

    let mut conn = swayipc::Connection::new()
        .map_err(|_| AppError::Conn)?;

    let tree = conn.get_tree()
        .map_err(|_| AppError::GetTree)?;
    let workspaces = conn.get_workspaces()
        .map_err(|_| AppError::GetWorkspaces)?;

    let focused_workspace = workspaces.iter()
        .find(|w| w.focused)
        .ok_or(AppError::NoFocus)?;
    let focused_workspace_node = find_by_id(&tree, focused_workspace.id)
        .ok_or(AppError::NoFocus)?;

    let to_balance = match arg_matches.get_flag("focus") {
        true => top_focus(focused_workspace_node).ok_or(AppError::NoFocus)?,
        false => focused_workspace_node,
    };
    
    balance(&mut conn, to_balance)
}

