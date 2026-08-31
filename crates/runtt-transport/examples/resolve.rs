//! Resolve a placement label against whatever is attached.
//!
//!     cargo run -p runtt-transport --example resolve -- usb:3-4
//!
//! Useful on the bench: it answers "would the runtime find this board, and
//! which channels would it get?" without deploying anything.
fn main() {
    let label = std::env::args().nth(1).unwrap_or_else(|| "usb:3-4".into());
    match runtt_transport::Target::parse(&label).map(|t| runtt_transport::resolve::resolve(&t)) {
        Ok(Ok(r)) => {
            use runtt_transport::resolve::Resolved;
            println!("{label}");
            match &r {
                Resolved::Serial { mgmt, .. } => println!("  mgmt: {}", mgmt.display()),
                Resolved::Can { iface, node_id, .. } => println!(
                    "  mgmt: {iface} isotp tx={node_id:#x} rx={:#x}",
                    node_id + 1
                ),
            }
            match r.log_source() {
                Some(runtt_transport::resolve::LogSource::Serial(l)) => {
                    println!("  log:  {}", l.display())
                }
                Some(runtt_transport::resolve::LogSource::Can { iface, id }) => {
                    println!("  log:  {iface} raw frames on {id:#x}")
                }
                None => println!("  log:  <none: single channel>"),
            }
        }
        Ok(Err(e)) => println!("{label} -> {e:#}"),
        Err(e) => println!("{label} -> {e:#}"),
    }
}
