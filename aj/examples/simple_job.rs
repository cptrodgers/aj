use std::time::Duration;

use aj::job;
use aj::{export::core::actix_rt::time::sleep, main, AJ};

#[job]
fn hello(number: i32, number2: i32) {
    println!("Hello {} {number2}", number);
}

#[job]
async fn async_hello(number: i32, number2: i32) {
    // We support async fn as well
    println!("Hello {} {number2}", number);
}

#[main]
async fn main() {
    // Start AJ engine
    AJ::quick_start();

    // Wait the job is registered in AJ
    let _ = hello::run(1, 2).await;

    // Or fire and forget it
    let _ = async_hello::just_run(3, 4);

    // Sleep 1 ms to view the result from job
    sleep(Duration::from_secs(1)).await;
}
