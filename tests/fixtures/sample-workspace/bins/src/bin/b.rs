//! `b` bin: does not use AppGreeter at all. Changes to `app::AppGreeter::greet`
//! should never report `b` as affected.

fn main() {
    println!("b");
}
