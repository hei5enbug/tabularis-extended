use std::ops::ControlFlow;

use sqlparser::ast::{Expr, Query, Select, SetExpr, Statement, TableFactor, Visit, Visitor};
use sqlparser::dialect::{
    Dialect, GenericDialect, MsSqlDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect,
};
use sqlparser::parser::Parser;

use crate::ai_activity;
use crate::drivers::driver_trait::SqlDialect;

pub const MAX_MANAGED_SQL_BYTES: usize = 256 * 1024;

pub fn validate_managed_select(sql: &str, dialect: SqlDialect) -> Result<(), &'static str> {
    if sql.len() > MAX_MANAGED_SQL_BYTES {
        return Err("QUERY_TOO_LARGE");
    }
    if ai_activity::classify_query_kind(sql) != "select" {
        return Err("INVALID_QUERY");
    }

    let parsed = match Parser::parse_sql(dialect_parser(dialect).as_ref(), sql) {
        Ok(parsed) => parsed,
        Err(_) => return Err("INVALID_QUERY"),
    };
    if parsed.len() != 1 {
        return Err("INVALID_QUERY");
    }
    let Statement::Query(root) = &parsed[0] else {
        return Err("INVALID_QUERY");
    };

    let mut validator = SelectValidator {
        valid: true,
        select_count: 0,
        query_depth: 0,
    };
    let _ = root.visit(&mut validator);
    if validator.valid && validator.select_count > 0 {
        Ok(())
    } else {
        Err("INVALID_QUERY")
    }
}

fn dialect_parser(dialect: SqlDialect) -> Box<dyn Dialect> {
    match dialect {
        SqlDialect::Postgres => Box::new(PostgreSqlDialect {}),
        SqlDialect::Mysql => Box::new(MySqlDialect {}),
        SqlDialect::Mssql => Box::new(MsSqlDialect {}),
        SqlDialect::Sqlite => Box::new(SQLiteDialect {}),
        SqlDialect::Oracle | SqlDialect::Generic => Box::new(GenericDialect {}),
    }
}

struct SelectValidator {
    valid: bool,
    select_count: usize,
    query_depth: usize,
}

impl Visitor for SelectValidator {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        let root_query = self.query_depth == 0;
        self.query_depth += 1;
        if !query.locks.is_empty()
            || (root_query && !is_select_root_set_expr(&query.body))
            || (!root_query && !is_safe_nested_set_expr(&query.body))
        {
            self.valid = false;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &Query) -> ControlFlow<Self::Break> {
        self.query_depth = self.query_depth.saturating_sub(1);
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<Self::Break> {
        self.select_count += 1;
        if select.into.is_some() {
            self.valid = false;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<Self::Break> {
        if !matches!(statement, Statement::Query(_)) {
            self.valid = false;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Self::Break> {
        const LOCK_HINTS: &[&str] = &["UPDLOCK", "HOLDLOCK", "XLOCK", "TABLOCKX"];
        if let TableFactor::Table { with_hints, .. } = factor {
            if with_hints.iter().any(|hint| match hint {
                Expr::Identifier(identifier) => LOCK_HINTS
                    .iter()
                    .any(|lock| identifier.value.eq_ignore_ascii_case(lock)),
                _ => false,
            }) {
                self.valid = false;
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }
}

fn is_select_root_set_expr(expr: &SetExpr) -> bool {
    match expr {
        SetExpr::Select(_) => true,
        SetExpr::Query(query) => query.locks.is_empty() && is_select_root_set_expr(&query.body),
        SetExpr::SetOperation { left, right, .. } => {
            is_select_root_set_expr(left) && is_select_root_set_expr(right)
        }
        SetExpr::Values(_)
        | SetExpr::Insert(_)
        | SetExpr::Update(_)
        | SetExpr::Delete(_)
        | SetExpr::Merge(_)
        | SetExpr::Table(_) => false,
    }
}

fn is_safe_nested_set_expr(expr: &SetExpr) -> bool {
    match expr {
        SetExpr::Select(_) | SetExpr::Values(_) => true,
        SetExpr::Query(query) => query.locks.is_empty() && is_safe_nested_set_expr(&query.body),
        SetExpr::SetOperation { left, right, .. } => {
            is_safe_nested_set_expr(left) && is_safe_nested_set_expr(right)
        }
        SetExpr::Insert(_)
        | SetExpr::Update(_)
        | SetExpr::Delete(_)
        | SetExpr::Merge(_)
        | SetExpr::Table(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 주어진_select_cte일때_허용한다() {
        // Given
        let sql = "WITH recent AS (SELECT id FROM orders) SELECT id FROM recent";
        // When
        let result = validate_managed_select(sql, SqlDialect::Postgres);
        // Then
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn 주어진_select_set_operation일때_허용한다() {
        // Given
        let sql = "SELECT 1 UNION ALL SELECT 2";
        // When
        let result = validate_managed_select(sql, SqlDialect::Postgres);
        // Then
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn 주어진_select_into일때_거부한다() {
        // Given
        let sql = "SELECT * INTO copied FROM source";
        // When
        let result = validate_managed_select(sql, SqlDialect::Mssql);
        // Then
        assert_eq!(result, Err("INVALID_QUERY"));
    }

    #[test]
    fn 주어진_데이터변경_cte일때_거부한다() {
        // Given
        let sql = "WITH changed AS (DELETE FROM users RETURNING id) SELECT id FROM changed";
        // When
        let result = validate_managed_select(sql, SqlDialect::Postgres);
        // Then
        assert_eq!(result, Err("INVALID_QUERY"));
    }

    #[test]
    fn 주어진_잠금_select일때_거부한다() {
        // Given
        let sql = "SELECT * FROM users FOR UPDATE";
        // When
        let result = validate_managed_select(sql, SqlDialect::Postgres);
        // Then
        assert_eq!(result, Err("INVALID_QUERY"));
    }

    #[test]
    fn 주어진_다중문장일때_거부한다() {
        // Given
        let sql = "SELECT 1; SELECT 2";
        // When
        let result = validate_managed_select(sql, SqlDialect::Postgres);
        // Then
        assert_eq!(result, Err("INVALID_QUERY"));
    }

    #[test]
    fn 주어진_values_only일때_거부한다() {
        // Given
        let sql = "VALUES (1), (2)";
        // When
        let result = validate_managed_select(sql, SqlDialect::Postgres);
        // Then
        assert_eq!(result, Err("INVALID_QUERY"));
    }

    #[test]
    fn 주어진_show와_explain일때_거부한다() {
        // Given
        let cases = ["SHOW TABLES", "EXPLAIN SELECT 1"];
        // When / Then
        for sql in cases {
            assert_eq!(
                validate_managed_select(sql, SqlDialect::Postgres),
                Err("INVALID_QUERY"),
                "{sql}"
            );
        }
    }

    #[test]
    fn 주어진_문법오류일때_거부한다() {
        // Given
        let sql = "SELECT FROM";
        // When
        let result = validate_managed_select(sql, SqlDialect::Postgres);
        // Then
        assert_eq!(result, Err("INVALID_QUERY"));
    }

    #[test]
    fn 주어진_256_kibibyte초과_sql일때_거부한다() {
        // Given
        let sql = format!("SELECT '{}'", "x".repeat(MAX_MANAGED_SQL_BYTES));
        // When
        let result = validate_managed_select(&sql, SqlDialect::Postgres);
        // Then
        assert_eq!(result, Err("QUERY_TOO_LARGE"));
    }

    #[test]
    fn 주어진_sqlserver_top_select일때_해당_dialect로_허용한다() {
        // Given
        let sql = "SELECT TOP (1) id FROM users";
        // When
        let result = validate_managed_select(sql, SqlDialect::Mssql);
        // Then
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn 주어진_select_from_values파생테이블일때_허용한다() {
        // Given
        let sql = "SELECT n FROM (VALUES (1), (2)) AS v(n)";
        // When
        let result = validate_managed_select(sql, SqlDialect::Postgres);
        // Then
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn 주어진_sqlserver잠금힌트일때_거부한다() {
        // Given
        let cases = [
            "SELECT id FROM users WITH (UPDLOCK)",
            "SELECT id FROM users WITH (HOLDLOCK)",
            "SELECT id FROM users WITH (XLOCK)",
            "SELECT id FROM users WITH (TABLOCKX)",
        ];
        // When / Then
        for sql in cases {
            assert_eq!(
                validate_managed_select(sql, SqlDialect::Mssql),
                Err("INVALID_QUERY"),
                "{sql}"
            );
        }
    }
}
