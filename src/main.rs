use std::collections::VecDeque;
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

/// For a given node id, get its info using a new swayipc call
fn get_latest_info(conn: &mut Connection, node_id: i64) 
-> Result<Node, AppError> {
    let tree = conn.get_tree().map_err(|_| AppError::GetTree)?;
    find_by_id(&tree, node_id).ok_or(AppError::NodeGone).cloned()
}

fn reorder(conn: &mut Connection, parent: Node) 
-> Result<Vec<(i64,i64)>, AppError> {
    let swaps_made: Vec<(i64, i64)> = Vec::new();

    type DimGetter = fn(&Node) -> i32;
    let (get_dim, get_pos): (DimGetter, DimGetter) = match parent.layout {
        NodeLayout::SplitH => (|n| n.rect.width, |n| n.rect.x),
        NodeLayout::SplitV => (|n| n.rect.height, |n| n.rect.y),
        _ => unreachable!("Handled in caller"),
    };

    let desired_dim = get_dim(&parent) / parent.nodes.len() as i32;
    eprintln!("Desired dim: {}", desired_dim);
    let (child1, child2) = (parent.nodes.get(0).unwrap(), parent.nodes.get(1).unwrap());
    let gaps_size = get_pos(child2) - (get_pos(child1) + get_dim(child1));

    // Once this returns None, all children are in a position such that 
    // resizing from left to right should be possible without errors
    let find_faulty = |children: &[Node]| -> Option<Node> { 
        for (idx, child) in children.iter().enumerate() {
            let gaps = gaps_size + (gaps_size as f32 * (idx as f32 + 0.5)).round() as i32;
            eprintln!("gaps: {:?}", gaps);
            let min_allowed = get_pos(&parent) + gaps + ((idx+1) as i32 * desired_dim);
            eprintln!("{:?}: {}+{} < {}", child.name,
                get_pos(child), get_dim(child), min_allowed
            );
            if get_pos(child) + get_dim(child) < min_allowed {
                return Some(child.clone())
            }
        }
        None
    };

    // Greedily child in faulty position with largest remaining sibling
    let mut parent = parent.clone();
    while let Some(child) = find_faulty(&parent.nodes) {
        let largest_remaining = parent.nodes.iter()
            .skip_while(|n| n.id != child.id).skip(1)
            .max_by(|x,y| i32::cmp(&get_dim(x), &get_dim(y)))
            .ok_or(AppError::Swap)?; // Better error?

        let cmd = format!("swaymsg [con_id={}] swap container with con_id {}",
            child.id, largest_remaining.id);

        let res = conn.run_command(cmd).map_err(|_| AppError::Swap)?;
        res.first()
            .ok_or(AppError::Resize)?
            .as_ref().map_err(|_|AppError::Resize)?;

        // Get the new state of the workspace after the swap
        parent = get_latest_info(conn, parent.id)
            .map_err(|_| AppError::NodeGone)?;
    }

    Ok(swaps_made)
}

fn balance2(conn: &mut Connection, root: &Node) -> Result<(), AppError> {
    let mut q: VecDeque<i64> = VecDeque::from(vec![root.id]);
    
    while let Some(cur_id) = q.pop_front() {
        if root.nodes.is_empty() { return Ok(()) }

        let cur = get_latest_info(conn, cur_id)?;
        if cur.nodes.is_empty() { continue }

        let initial_order = cur.nodes.iter()
            .map(|Node { id, ..}| *id)
            .collect::<Vec<i64>>();

        reorder(conn, cur.clone())?;
        // balance
        
        q.extend(cur.nodes.iter().map(|n| n.id));
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
    
    balance2(&mut conn, to_balance)
}

