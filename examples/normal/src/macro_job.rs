use std::time::Duration;

use aj::export::core::actix_rt::time::sleep;
use aj::job;

#[job]
fn hello(name: String) {
    println!("Hello {name}");
}

#[job]
async fn async_hello(name: String) {
    // We support async fn as well
    println!("Hello async, {name}");
}

pub async fn run() {
    // Wait the job is registered in AJ
    let _ = hello::run("Rodgers".into()).await;

    // Or fire and forget it
    async_hello::just_run("AJ".into());

    // Sleep 1 ms to view the result from job
    sleep(Duration::from_secs(1)).await;
}
