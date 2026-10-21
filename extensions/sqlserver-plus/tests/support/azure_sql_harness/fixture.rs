use super::{config::Config, Error, Result};
use serde_json::{json, Value};

pub const DECIMAL: &str = "12345678901234567890.123456789012345678";
pub const OBSERVED_AT: &str = "2030-01-02T03:04:05+09:00";
pub fn table(config: &Config) -> Value {
    json!({"database":config.database,"schema":config.schema,"table":config.table})
}
pub fn qualified(config: &Config) -> String {
    format!("[{}].[{}]", config.schema, config.table)
}
pub fn identity(run_id: &str, id: i32) -> Value {
    json!({"columns":["run_id","id"],"values":[run_id,id]})
}
pub fn values(run_id: &str, id: i32) -> Value {
    match id {
        1 => {
            json!({"run_id":run_id,"id":id,"value":"original","measure":DECIMAL,"observed_at":OBSERVED_AT})
        }
        2 => json!({"run_id":run_id,"id":id,"value":null,"measure":null,"observed_at":null}),
        _ => {
            json!({"run_id":run_id,"id":id,"value":"한글","measure":"0.000000000000000001","observed_at":OBSERVED_AT})
        }
    }
}
pub fn exact(text: String, run_id: &str) -> Value {
    json!({"text":text,"parameters":[{"name":"@run","value":run_id}],"max_rows":100,"max_bytes":1024*1024})
}
pub fn all_rows(config: &Config, run_id: &str) -> Value {
    exact(
        format!(
            "SELECT id,value,measure,observed_at FROM {} WHERE run_id=@run ORDER BY id",
            qualified(config)
        ),
        run_id,
    )
}
pub fn owned_row(config: &Config, run_id: &str, id: i32) -> Value {
    json!({"text":format!("SELECT run_id,id,value FROM {} WHERE run_id=@run AND id=@id",qualified(config)),"parameters":[{"name":"@run","value":run_id},{"name":"@id","value":id}],"max_rows":2,"max_bytes":65536})
}
pub fn validate_metadata(value: &Value) -> Result<()> {
    let columns = value["columns"]
        .as_array()
        .ok_or(Error("FIXTURE_METADATA_MISSING"))?;
    let expected = [
        ("run_id", "nvarchar", false, Some(1)),
        ("id", "int", false, Some(2)),
        ("value", "nvarchar", true, None),
        ("measure", "decimal", true, None),
        ("observed_at", "datetimeoffset", true, None),
    ];
    if columns.len() != expected.len() {
        return Err(Error("FIXTURE_METADATA_MISMATCH"));
    }
    for (column, (name, dtype, nullable, pk)) in columns.iter().zip(expected) {
        if column["name"] != name
            || column["data_type"] != dtype
            || column["is_nullable"] != nullable
            || column["is_identity"] != false
            || column["is_pk"] != pk.is_some()
            || column["primary_key_position"].as_u64() != pk
        {
            return Err(Error("FIXTURE_METADATA_MISMATCH"));
        }
    }
    if columns[0]["max_length"] != 72
        || columns[2]["max_length"] != 200
        || columns[3]["precision"] != 38
        || columns[3]["scale"] != 18
        || columns[4]["scale"] != 7
    {
        return Err(Error("FIXTURE_METADATA_MISMATCH"));
    }
    Ok(())
}
pub fn affected(value: &Value) -> bool {
    value["affected_rows"] == 1 && value["truncated"] != true
}
pub fn original_rows(value: &Value) -> bool {
    value["columns"] == json!(["id", "value", "measure", "observed_at"])
        && value["rows"]
            == json!([
                [1, "original", DECIMAL, OBSERVED_AT],
                [2, null, null, null],
                [3, "한글", "0.000000000000000001", OBSERVED_AT]
            ])
        && value["truncated"] != true
}
