use domain::Greeter;

pub struct AppGreeter;

impl Greeter for AppGreeter {
    fn greet(&self) -> String {
        "hello from AppGreeter".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_greet_returns_expected_string() {
        let g = AppGreeter;
        assert_eq!(g.greet(), "hello from AppGreeter");
    }

    #[test]
    fn test_unrelated_arithmetic() {
        assert_eq!(2 + 2, 4);
    }
}
