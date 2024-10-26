use std::time::Duration;

use aj::job;
use aj::{export::core::actix_rt::time::sleep, AJ};

#[job]
fn hello(name: String) {
    println!("Hello {name}");
}

#[job]
async fn async_hello(name: String) {
    // We support async fn as well
    println!("Hello async, {name}");
}

#[tokio::main]
async fn main() {
    // Start AJ engine
    std::thread::spawn(|| {
        AJ::quick_start();
    }).join().expect("Panicked thread");

    // Wait the job is registered in AJ
    let _ = hello::run("Rodgers".into()).await;

    // Or fire and forget it
    let _ = async_hello::just_run("AJ".into());

    // Sleep 1 ms to view the result from job
    tokio::time::sleep(Duration::from_secs(1)).await;
}
