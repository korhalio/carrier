extern crate carrier;
extern crate tokio_core;

use carrier::service;

use tokio_core::reactor::Core;

use std::env::args;
use std::net::SocketAddr;

fn main() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let mut evt_loop = Core::new().unwrap();
    let server_addr: SocketAddr = ([127, 0, 0, 1], 22222).into();

    let name = args()
        .nth(1)
        .expect("Please give the name of the other peer.");

    let builder = carrier::Peer::build(
        &evt_loop.handle(),
        format!("{}/src/bin/cert.pem", manifest_dir),
        format!("{}/src/bin/key.pem", manifest_dir),
        "dev".into(),
    ).unwrap().login(&server_addr, "test");

    let peer = evt_loop.run(builder).unwrap();

    peer.run_service(&mut evt_loop, service::lifeline::Lifeline::new(), &name)
        .unwrap()
}
