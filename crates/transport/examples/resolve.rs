//! Resolve a placement label against whatever is attached.
//!
//!     cargo run -p transport --example resolve -- usb:3-4
//!
//! Useful on the bench: it answers "would the runtime find this board, and
//! which channels would it get?" without deploying anything.
fn main() {
    let label = std::env::args().nth(1).unwrap_or_else(|| "usb:3-4".into());
    match transport::Target::parse(&label).map(|t| transport::resolve::resolve(&t)) {
        Ok(Ok(r)) => {
            println!("{label}");
            println!("  mgmt: {}", r.mgmt.display());
            match r.log {
                Some(l) => println!("  log:  {}", l.display()),
                None => println!("  log:  <none: single channel>"),
            }
        }
        Ok(Err(e)) => println!("{label} -> {e:#}"),
        Err(e) => println!("{label} -> {e:#}"),
    }
}
