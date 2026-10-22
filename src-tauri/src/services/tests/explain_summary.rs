use super::plan_reply;
use crate::services::contracts::ErrorCode;
use serde_json::{json, Value};

const POSTGRES_PLAN: &str = r#"[{"Plan":{"Node Type":"Hash Join","Startup Cost":10.5,"Total Cost":152.3,"Plan Rows":120,"Plans":[{"Node Type":"Seq Scan","Relation Name":"orders","Schema":"sales","Total Cost":80.0,"Plan Rows":1000},{"Node Type":"Hash","Total Cost":20,"Plan Rows":50,"Plans":[{"Node Type":"Index Scan","Relation Name":"customers","Index Name":"customers_pkey","Total Cost":15.1,"Plan Rows":50}]}]}}]"#;

const SHOWPLAN: &str = r#"<?xml version="1.0"?>
<ShowPlanXML xmlns="http://schemas.microsoft.com/sqlserver/2004/07/showplan" Version="1.6">
 <BatchSequence><Batch><Statements>
  <StmtSimple StatementText="SELECT 1" StatementSubTreeCost="1.25" StatementEstRows="120" StatementOptmEarlyAbortReason="TimeOut">
   <QueryPlan>
    <MissingIndexes>
     <MissingIndexGroup Impact="87.5">
      <MissingIndex Database="[shop]" Schema="[dbo]" Table="[orders]">
       <ColumnGroup Usage="EQUALITY"><Column Name="[status]" ColumnId="3"/></ColumnGroup>
       <ColumnGroup Usage="INEQUALITY"><Column Name="[created_at]" ColumnId="4"/></ColumnGroup>
       <ColumnGroup Usage="INCLUDE"><Column Name="[customer_id]" ColumnId="2"/><Column Name="[total]" ColumnId="5"/></ColumnGroup>
      </MissingIndex>
     </MissingIndexGroup>
    </MissingIndexes>
    <Warnings NoJoinPredicate="true">
     <PlanAffectingConvert ConvertIssue="Seek Plan" Expression="CONVERT_IMPLICIT(int,[o].[status],0)"/>
     <ColumnsWithNoStatistics><ColumnReference Column="status"/></ColumnsWithNoStatistics>
    </Warnings>
    <RelOp NodeId="0" PhysicalOp="Nested Loops" LogicalOp="Inner Join" EstimateRows="120" EstimatedTotalSubtreeCost="1.25">
     <NestedLoops>
      <RelOp NodeId="1" PhysicalOp="Clustered Index Scan" LogicalOp="Clustered Index Scan" EstimateRows="120" EstimatedTotalSubtreeCost="0.9">
       <IndexScan><Object Database="[shop]" Schema="[dbo]" Table="[orders]" Index="[PK_orders]"/></IndexScan>
      </RelOp>
      <RelOp NodeId="2" PhysicalOp="Index Seek" LogicalOp="Index Seek" EstimateRows="1" EstimatedTotalSubtreeCost="0.003">
       <IndexScan><Object Database="[shop]" Schema="[dbo]" Table="[customers]" Index="[PK_customers]"/></IndexScan>
      </RelOp>
     </NestedLoops>
    </RelOp>
   </QueryPlan>
  </StmtSimple>
 </Statements></Batch></BatchSequence>
</ShowPlanXML>"#;

fn postgres_reply(payload: Value) -> Value {
    plan_reply(&json!({"engine":"postgres","format":"postgres-json","payload":payload})).unwrap()
}
fn sqlserver_reply(payload: Value) -> Value {
    plan_reply(&json!({"engine":"sqlserver","format":"sqlserver-showplan-xml","payload":payload}))
        .unwrap()
}

#[test]
fn postgresql_계획은_비용과_노드와_전체_스캔을_요약한다() {
    // given
    let payload = json!(POSTGRES_PLAN);

    // when
    let actual = postgres_reply(payload);

    // then
    let summary = &actual["summary"];
    assert_eq!(actual["engine"], "postgres");
    assert_eq!(actual["format"], "postgres-json");
    assert_eq!(actual["executed"], false);
    assert_eq!(summary["cost_unit"], "postgres_planner_cost");
    assert_eq!(summary["total_cost"], 152.3);
    assert_eq!(summary["estimated_rows"], 120);
    assert_eq!(summary["node_count"], 4);
    let operations: Vec<&str> = summary["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["operation"].as_str().unwrap())
        .collect();
    assert_eq!(operations, ["Hash Join", "Seq Scan", "Hash", "Index Scan"]);
    let depths: Vec<u64> = summary["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["depth"].as_u64().unwrap())
        .collect();
    assert_eq!(depths, [0, 1, 1, 2]);
    assert_eq!(summary["nodes"][3]["index"], "customers_pkey");
    assert_eq!(summary["nodes"][3]["object"], "customers");
    assert_eq!(
        summary["full_scans"],
        json!([{"operation":"Seq Scan","object":"sales.orders","estimated_rows":1000}])
    );
    assert_eq!(summary["warnings"], json!([]));
    assert_eq!(summary["missing_indexes"], json!([]));
    assert_eq!(actual["plan"][0]["Plan"]["Node Type"], "Hash Join");
    assert!(actual.get("plan_omitted").is_none());
}

#[test]
fn postgresql_계획은_문자열로_온_json도_같은_요약을_준다() {
    // given
    let text = json!(POSTGRES_PLAN);
    let value: Value = serde_json::from_str(POSTGRES_PLAN).unwrap();

    // when
    let from_text = postgres_reply(text);
    let from_value = postgres_reply(value);

    // then
    assert_eq!(from_text["summary"], from_value["summary"]);
    assert_eq!(from_text["plan"], from_value["plan"]);
}

#[test]
fn sqlserver_계획은_비용과_스캔과_경고와_누락_인덱스를_요약한다() {
    // given
    let payload = json!(SHOWPLAN);

    // when
    let actual = sqlserver_reply(payload);

    // then
    let summary = &actual["summary"];
    assert_eq!(summary["cost_unit"], "sqlserver_estimated_subtree_cost");
    assert_eq!(summary["total_cost"], 1.25);
    assert_eq!(summary["estimated_rows"], 120);
    assert_eq!(summary["node_count"], 3);
    assert_eq!(summary["nodes"][0]["operation"], "Nested Loops");
    assert_eq!(summary["nodes"][0]["depth"], 0);
    assert_eq!(summary["nodes"][1]["depth"], 1);
    assert_eq!(summary["nodes"][1]["object"], "[dbo].[orders]");
    assert_eq!(summary["nodes"][1]["index"], "[PK_orders]");
    assert_eq!(summary["nodes"][2]["operation"], "Index Seek");
    assert_eq!(
        summary["full_scans"],
        json!([{"operation":"Clustered Index Scan","object":"[dbo].[orders]","estimated_rows":120}])
    );
    let warnings: Vec<&str> = summary["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|warning| warning.as_str().unwrap())
        .collect();
    assert!(warnings.contains(&"StatementOptmEarlyAbortReason=TimeOut"));
    assert!(warnings.contains(&"NoJoinPredicate"));
    assert!(warnings.contains(&"PlanAffectingConvert (ConvertIssue=Seek Plan)"));
    assert!(warnings.contains(&"ColumnsWithNoStatistics"));
    assert_eq!(
        summary["missing_indexes"],
        json!([{
            "impact":87.5,
            "table":"[shop].[dbo].[orders]",
            "equality_columns":["[status]"],
            "inequality_columns":["[created_at]"],
            "include_columns":["[customer_id]","[total]"],
        }])
    );
    assert_eq!(actual["plan"], json!(SHOWPLAN));
}

#[test]
fn 읽을_수_없는_계획은_읽은_만큼만_요약하고_원문을_그대로_둔다() {
    // given
    let broken_xml = json!("<ShowPlanXML><RelOp PhysicalOp=\"Table Scan\" EstimateRows=\"7\"");
    let unreadable_cost = json!("<ShowPlanXML><RelOp PhysicalOp=\"Table Scan\" EstimateRows=\"7\" EstimatedTotalSubtreeCost=\"abc\"/></ShowPlanXML>");
    let not_json = json!("not json");
    let cases = [
        (true, json!(null)),
        (true, json!(42)),
        (true, json!({"Plan":"x"})),
        (
            true,
            json!([{"Plan":{"Node Type":7,"Plans":"x","Total Cost":"cheap"}}]),
        ),
        (true, not_json),
        (false, json!(null)),
        (false, json!(["x"])),
        (false, broken_xml),
        (false, unreadable_cost),
    ];

    // when
    let actual: Vec<Value> = cases
        .iter()
        .map(|(postgres, payload)| {
            if *postgres {
                postgres_reply(payload.clone())
            } else {
                sqlserver_reply(payload.clone())
            }
        })
        .collect();

    // then
    for reply in &actual {
        assert_eq!(reply["executed"], false);
        assert!(reply["summary"]["nodes"].is_array());
        assert_eq!(reply["summary"]["warnings"], json!([]));
    }
    assert_eq!(actual[0]["summary"]["node_count"], 0);
    assert_eq!(actual[0]["summary"]["total_cost"], Value::Null);
    assert!(actual[0].get("plan").is_none() && actual[0].get("plan_omitted").is_none());
    assert_eq!(actual[3]["summary"]["node_count"], 1);
    assert_eq!(actual[3]["summary"]["total_cost"], Value::Null);
    assert_eq!(actual[4]["plan"], json!("not json"));
    assert_eq!(actual[7]["summary"]["node_count"], 0);
    assert_eq!(actual[8]["summary"]["node_count"], 1);
    assert_eq!(actual[8]["summary"]["estimated_rows"], 7);
    assert_eq!(actual[8]["summary"]["total_cost"], Value::Null);
    assert_eq!(
        actual[8]["summary"]["full_scans"][0]["operation"],
        "Table Scan"
    );
}

#[test]
fn 세미콜론_없이_이어진_두_문장의_showplan은_잘못된_인자로_거부한다() {
    // given
    let two = SHOWPLAN.replace(
        "</StmtSimple>\n </Statements>",
        "</StmtSimple>\n  <StmtSimple StatementText=\"SELECT 2\" StatementSubTreeCost=\"0.1\"/>\n </Statements>",
    );
    assert_ne!(two, SHOWPLAN);
    let raw = json!({"engine":"sqlserver","format":"sqlserver-showplan-xml","payload":two});

    // when
    let actual = plan_reply(&raw).err().map(|error| error.code);

    // then
    assert_eq!(actual, Some(ErrorCode::InvalidArgument));
}

#[test]
fn 알_수_없는_형식이나_엔진_누락은_잘못된_인자로_거부한다() {
    // given
    let cases = [
        json!({"engine":"mysql","format":"mysql-json","payload":"{}"}),
        json!({"engine":"postgres","payload":"[]"}),
        json!({"format":"postgres-json","payload":"[]"}),
        json!({"engine":1,"format":"postgres-json","payload":"[]"}),
        json!("plan"),
        Value::Null,
    ];

    // when
    let actual: Vec<Option<ErrorCode>> = cases
        .iter()
        .map(|raw| plan_reply(raw).err().map(|error| error.code))
        .collect();

    // then
    assert!(actual
        .iter()
        .all(|code| *code == Some(ErrorCode::InvalidArgument)));
}

#[test]
fn 계획_원문은_삼십이_키비바이트_이하일_때만_담고_아니면_생략을_알린다() {
    // given
    let small = format!("<ShowPlanXML>{}</ShowPlanXML>", " ".repeat(30 * 1024));
    let large = format!("<ShowPlanXML>{}</ShowPlanXML>", " ".repeat(33 * 1024));
    let large_json = json!([{"Plan":{"Node Type":"Result","Note":"x".repeat(40 * 1024)}}]);

    // when
    let kept = sqlserver_reply(json!(small));
    let omitted = sqlserver_reply(json!(large));
    let omitted_json = postgres_reply(large_json);

    // then
    assert!(kept.get("plan").is_some() && kept.get("plan_omitted").is_none());
    assert!(omitted.get("plan").is_none());
    assert_eq!(omitted["plan_omitted"], true);
    assert!(omitted_json.get("plan").is_none());
    assert_eq!(omitted_json["plan_omitted"], true);
    assert_eq!(omitted_json["summary"]["node_count"], 1);
}

#[test]
fn 큰_계획도_노드는_사십개_스캔은_사십개로_자르고_답은_예산_안에_둔다() {
    // given
    let mut plan = json!({"Node Type":"Seq Scan","Relation Name":"t".repeat(300),"Total Cost":1.0,"Plan Rows":1});
    for _ in 0..120 {
        plan = json!({"Node Type":"Seq Scan","Relation Name":"t".repeat(300),"Total Cost":1.0,"Plan Rows":1,"Plans":[plan]});
    }
    let nodes: String = (0..400)
        .map(|n| format!("<RelOp NodeId=\"{n}\" PhysicalOp=\"Table Scan\" EstimateRows=\"1\" EstimatedTotalSubtreeCost=\"1\"><TableScan><Object Schema=\"[dbo]\" Table=\"[{}]\"/></TableScan></RelOp>", "t".repeat(300)))
        .collect();
    let xml = format!("<ShowPlanXML><Root>{nodes}</Root></ShowPlanXML>");

    // when
    let postgres = postgres_reply(json!([{"Plan":plan}]));
    let sqlserver = sqlserver_reply(json!(xml));

    // then
    for reply in [&postgres, &sqlserver] {
        assert_eq!(reply["summary"]["nodes"].as_array().unwrap().len(), 40);
        assert_eq!(reply["summary"]["full_scans"].as_array().unwrap().len(), 40);
        assert!(serde_json::to_vec(reply).unwrap().len() <= 56 * 1024);
    }
    assert_eq!(postgres["summary"]["node_count"], 121);
    assert_eq!(sqlserver["summary"]["node_count"], 400);
    assert_eq!(
        postgres["summary"]["nodes"][0]["object"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        96
    );
}

#[test]
fn 아주_깊은_xml과_json도_패닉없이_요약한다() {
    // given
    let depth = 5000;
    let xml = format!(
        "<ShowPlanXML>{}{}</ShowPlanXML>",
        "<RelOp PhysicalOp=\"Sort\" EstimateRows=\"1\" EstimatedTotalSubtreeCost=\"1\">"
            .repeat(depth),
        "</RelOp>".repeat(depth)
    );
    let json_text = format!(
        "{}{}",
        "[{\"Plan\":".repeat(depth),
        "{}".to_owned() + &"}]".repeat(depth)
    );

    // when
    let from_xml = sqlserver_reply(json!(xml));
    let from_json = postgres_reply(json!(json_text));

    // then
    assert_eq!(from_xml["summary"]["node_count"], 0);
    assert!(from_json["summary"]["nodes"].is_array());
}

#[test]
fn 백단계로_중첩된_계획은_모두_읽는다() {
    // given
    let depth = 100;
    let xml = format!(
        "<ShowPlanXML>{}{}</ShowPlanXML>",
        "<RelOp PhysicalOp=\"Sort\" EstimateRows=\"1\" EstimatedTotalSubtreeCost=\"1\">"
            .repeat(depth),
        "</RelOp>".repeat(depth)
    );

    // when
    let actual = sqlserver_reply(json!(xml));

    // then
    assert_eq!(actual["summary"]["node_count"], 100);
    assert_eq!(actual["summary"]["nodes"][39]["depth"], 39);
}
