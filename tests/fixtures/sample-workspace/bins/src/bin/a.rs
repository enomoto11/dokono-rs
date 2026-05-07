//! `a` bin: uses AppGreeter **through trait dispatch**.
//! When `app::AppGreeter::greet` changes, only `a` should be reported as
//! affected (`b` does not use it).

use std::sync::Arc;

use app::AppGreeter;
use domain::Greeter;

fn main() {
    let g: Arc<dyn Greeter> = Arc::new(AppGreeter);
    println!("{}", g.greet());
}
