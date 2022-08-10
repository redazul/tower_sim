use tower_sim::network;

fn main() {
    let mut network = network::Network::default();
    for slot in 0..1_000_000 {
        println!("slot {}", slot);
        network.step();
        println!("root {:?}", network.root());
    }
}
