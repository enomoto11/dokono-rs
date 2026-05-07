/// Small trait used to exercise references resolved through trait dispatch.
pub trait Greeter {
    fn greet(&self) -> String;
}
