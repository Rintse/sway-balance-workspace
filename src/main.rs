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
    #[error("Error issuing reize command") ]
    Resize,
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


enum ResizeResult { Success, NoSpace }

fn resize_node(conn: &mut Connection, n: &Node, parent: &Node) 
-> Result<ResizeResult, AppError> {
    let Node{ id: child_id, .. } = n;
    let Node{ nodes: children, .. } = parent;

    let (get_dim, dir): (fn(&Node) -> i32, &str)= match parent.layout {
        NodeLayout::SplitH => (|n| n.rect.width, "right"),
        NodeLayout::SplitV => (|n| n.rect.height, "down"),
        _ => return Ok(ResizeResult::Success),
    };

    let desired_dim = get_dim(parent) / children.len() as i32;
    let diff = desired_dim - get_dim(n);

    let change = if diff < 0 { "shrink" } else { "grow" };
    let diff = diff.abs();

    let cmd = format!("[con_id={child_id}] resize {change} {dir} {diff} px");
    let res = conn.run_command(cmd).map_err(|_| AppError::Resize)?;

    match res.first().ok_or(AppError::Resize)? {
        Ok(_)                   => Ok(ResizeResult::Success),
        Err(CommandParse(_))    => Ok(ResizeResult::NoSpace),
        Err(_)                  => Err(AppError::Resize),
    }
}

fn get_latest_info(conn: &mut Connection, id: i64) -> Result<Node, AppError> {
    let tree = conn.get_tree().map_err(|_| AppError::GetTree)?;
    find_by_id(&tree, id).ok_or(AppError::NodeGone).cloned()
}

fn balance(conn: &mut Connection, root: &Node) -> Result<(), AppError> {
    let mut q: VecDeque<i64> = VecDeque::from(vec![root.id]);

    while let Some(cur_id) = q.pop_front() {
        if root.nodes.is_empty() { return Ok(()) }

        let cur = get_latest_info(conn, cur_id)?;
        if cur.nodes.is_empty() { continue }

        // Resizing to the required size might fail for some of the nodes 
        // So we just retry the resizing until it succeeds or until we have
        // done an iteration for each child
        // TODO: is that always enough iterations?
        for _ in 0..cur.nodes.len() {
            let mut succeeded = true;
            // Once all except the last been resized, 
            // the last one should already have the right size
            let all_except_last = cur.nodes.iter().take(cur.nodes.len()-1);

            for Node { id: child_id, .. } in all_except_last {
                let child = get_latest_info(conn, *child_id)?;
                if let ResizeResult::NoSpace = resize_node(conn, &child, &cur)? {
                    succeeded = false;
                }
            }
            if succeeded { break }
        }
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

    // UNWRAP: There should always be at least one active workspace, right?
    let active_workspace = workspaces.iter()
        .find(|w| w.focused)
        .ok_or(AppError::NoFocus)?;
    let ws_node = find_by_id(&tree, active_workspace.id)
        .ok_or(AppError::NoFocus)?;

    let to_balance = match arg_matches.get_flag("focus") {
        true => top_focus(ws_node).ok_or(AppError::NoFocus)?,
        false => ws_node,
    };
    
    balance(&mut conn, to_balance)
}

