
```rust
use std::time::Duration;

use aj::{
    async_trait,
    main,
    BackgroundJob, Executable, JobBuilder, JobContext, AJ,
    export::core:: {
        actix_rt::time::sleep,
        serde::{Deserialize, Serialize},
    }
};

#[derive(BackgroundJob, Serialize, Deserialize, Debug, Clone)]
pub struct AJob;

#[async_trait]
impl Executable for AJob {
    type Output = ();

    async fn execute(&self, _context: &JobContext) -> Self::Output {
        println!("Hello Job");
    }
}

#[main]
async fn main() {
    // Start AJ engine with in memory backend.
    AJ::quick_start();

    // Run a Job in Background
    let message = AJob;
    let _ = message
        .job_builder()
        .build()
        .unwrap()
        .run_background()
        .await;

    // Wait this thread in 1 sec to view result in background job
    sleep(Duration::from_secs(1)).await;
}
```

[More examples](https://github.com/cptrodgers/aj/tree/master/aj/examples)
