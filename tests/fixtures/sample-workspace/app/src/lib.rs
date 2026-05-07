use domain::Greeter;

pub struct AppGreeter;

impl Greeter for AppGreeter {
    fn greet(&self) -> String {
        "hello from AppGreeter".to_string()
    }
}
