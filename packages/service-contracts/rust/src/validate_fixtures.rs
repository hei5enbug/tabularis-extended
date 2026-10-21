use serde::Deserialize;
use serde_json::Value;
use tabularis_service_contracts::contracts::{SchemaValidator, VALIDATION_FIXTURES};

#[derive(Deserialize)]
struct Fixture {
    schema: String,
    value: Value,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let validator = SchemaValidator::new()?;
    let fixtures: Vec<Fixture> = serde_json::from_str(VALIDATION_FIXTURES)?;
    let results: Vec<bool> = fixtures.iter().map(|fixture| match fixture.schema.as_str() {
        "request" => validator.validate_request(&fixture.value).is_ok(),
        "response" => validator.validate_response(&fixture.value).is_ok(),
        _ => false,
    }).collect();
    println!("{}", serde_json::to_string(&results)?);
    Ok(())
}
