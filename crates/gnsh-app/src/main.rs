#[allow(dead_code)]
mod storage;

fn main() {
    let _ = storage::ConsoleStore::default_store();
    println!("gnsh-play bootstrap");
}
