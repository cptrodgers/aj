use aj::{
    actix_rt::time::sleep, async_trait::async_trait, chrono::Duration, get_now_as_secs, serde::{Deserialize, Serialize}, BackgroundJob, Executable, JobBuilder, JobContext, AJ,
    main
};

#[derive(BackgroundJob, Serialize, Deserialize, Debug, Clone)]
pub struct Print {
    number: i32,
}

#[async_trait]
impl Executable for Print {
    type Output = ();

    async fn execute(&self, _context: &JobContext) -> Self::Output {
        println!("Inside: {}", get_now_as_secs());
        println!("Print {}", self.number);
    }
}


#[main]
async fn main() {
    AJ::quick_start();
    // Spawn a thread and wait 1 sec to view
    let job_id = Print {
        number: 1,
    }.job_builder().delay(Duration::seconds(1)).build().unwrap().run_background().await.unwrap();
    let _ = AJ::cancel_job::<Print>(job_id).await;

    println!("Before run: {}", get_now_as_secs());
    Print {
        number: 2,
    }.job_builder().delay(Duration::seconds(1)).build().unwrap().run_background().await.unwrap();

    sleep(std::time::Duration::from_secs(2)).await;
}
