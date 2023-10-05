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

/// Find the highest level node that is focused. 
/// This should be the "largest" container that is focused
fn top_focus(root: &Node) -> Option<&Node> {
    bfsearch(root, |n| n.focused)
}


/// For a given node id, get its info using a new swayipc call
/// Calling swayipc each time we do this makes sense at the moment because we 
/// only use info about one node once before altering the state again.
fn get_latest_info(conn: &mut Connection, node_id: i64) 
-> Result<Node, AppError> {
    let tree = conn.get_tree().map_err(|_| AppError::GetTree)?;
    find_by_id(&tree, node_id).ok_or(AppError::NodeGone).cloned()
}

fn balance(conn: &mut Connection, root: &Node) -> Result<(), AppError> {
    let mut q: VecDeque<i64> = VecDeque::from(vec![root.id]);

    while let Some(cur_id) = q.pop_front() {
        let cur = get_latest_info(conn, cur_id)?;
        if cur.nodes.is_empty() { continue }

        let (get_dim, dir): (fn(&Node) -> i32, &str)= match cur.layout {
            NodeLayout::SplitH => (|n| n.rect.width, "right"),
            NodeLayout::SplitV => (|n| n.rect.height, "down"),
            _ => break,
        };

        let sum_dim: i32 = cur.nodes.iter().map(get_dim).sum();
        let desired_dim = sum_dim / cur.nodes.len() as i32;
        // This should happen at most (\Sum_{k=1}^{num_of_children} k) times
        let n = cur.nodes.len() as f64;
        let max_iterations = (0.5 * n * (n + 1.0)).round() as usize;
        for _ in 1..max_iterations {
            // Loop until we were able to resize all children to the requested
            // size. This may take multiple tries if there is not enough space
            // in the adjacent container to grow into.
            let mut succeeded = true;

            // Once all except the last been resized, 
            // the last one should already have the right size
            let all_except_last = cur.nodes.iter()
                .take(cur.nodes.len()-1)
                .map(|Node {id,..}| id);

            for child_id in all_except_last {
                let child = get_latest_info(conn, *child_id).unwrap();
                let diff = desired_dim - get_dim(&child);

                let change = if diff < 0 { "shrink" } else { "grow" };
                let diff = diff.abs();

                let child_id = child.id;
                let cmd = format!("[con_id={child_id}] resize {change} {dir} {diff} px");

                // run_command returns a Result<Vec<Result<_,_>>,_>.
                // The outermost result indicates whether executing the command 
                // went wrong in some way. The innermost vector of results
                // indicates, for each command, the result of executing the 
                // command. The outermost Result may not go wrong here
                let res = conn.run_command(cmd).map_err(|_| AppError::Resize)?;

                // The innermost command can only be of the "cannot resize" type
                // any other error is unexpected and should propegate
                if let Err(e) = res.first().unwrap() {
                    match e {
                        CommandParse(e) => match e.as_str() {
                            "Cannot resize any further" => succeeded = false,
                            _ => return Err(AppError::Resize),
                        },
                        _ => return Err(AppError::Resize),
                    }
                };
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

