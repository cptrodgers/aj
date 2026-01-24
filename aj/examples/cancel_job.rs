use aj::{
    async_trait,
    export::core::{
        chrono::Duration,
        get_now_as_secs,
        serde::{Deserialize, Serialize},
    },
    BackgroundJob, Executable, JobContext, AJ,
};

#[derive(BackgroundJob, Serialize, Deserialize, Debug, Clone)]
pub struct Print {
    number: i32,
}

#[async_trait]
impl Executable for Print {
    type Output = ();

    async fn execute(&mut self, _context: &JobContext) -> Self::Output {
        println!("Inside: {}", get_now_as_secs());
        println!("Print {}", self.number);
    }
}

#[tokio::main]
async fn main() {
    AJ::quick_start();
    let job_id = Print { number: 1 }
        .job()
        .delay(Duration::seconds(1))
        .run()
        .await
        .unwrap();
    let _ = AJ::cancel_job::<Print>(job_id).await;

    println!("Before run: {}", get_now_as_secs());
    Print { number: 2 }
        .job()
        .delay(Duration::seconds(1))
        .run()
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
}
