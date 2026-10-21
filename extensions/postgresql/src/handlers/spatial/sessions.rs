use serde::Deserialize;
use serde_json::{json,Value};

use crate::{cancel::ScopedGuard,session};
use super::{connection::{Lease,verify_transaction},types::{self,DriverContext},SpatialError};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenInput {database: Option<String>,mode: Mode}
#[derive(Deserialize,PartialEq)]
#[serde(rename_all="snake_case")]
enum Mode {ReadOnly,Write}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FinishInput {action: Action}
#[derive(Deserialize,PartialEq)]
#[serde(rename_all="snake_case")]
enum Action {Commit,Rollback,Close}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

pub async fn dispatch(id: Value,params: &Value,method: &str) -> Value {
    dispatch_with_scope(id,params,method,None).await
}
pub async fn dispatch_with_scope(id: Value,params: &Value,method: &str,scope: Option<ScopedGuard>) -> Value {
    match run(params,method,scope).await {Ok(value)=>crate::rpc::ok_response(id,value),Err(error)=>error.response(id)}
}
async fn run(params: &Value,method: &str,scope: Option<ScopedGuard>) -> Result<Value,SpatialError> {
    let (context,mut connection)=DriverContext::parse(params,false)?;
    let scope=match scope {Some(scope)=>scope,None=>ScopedGuard::reserve(&context.connection_id,&context.request_id)?};
    if method=="service_session_open" {
        let input: OpenInput=types::input(params)?;
        if context.session_handle.is_some() || (input.mode==Mode::ReadOnly)!=context.read_only {
            return Err(SpatialError::argument("Session open context does not match the requested mode"));
        }
        if let Some(database)=input.database {connection.database=Some(database);}
        let mut lease=tokio::time::timeout(std::time::Duration::from_millis(context.deadline_ms),Lease::acquire(&context,&connection,&scope)).await
            .map_err(|_|SpatialError::new("DEADLINE_EXCEEDED","Session acquisition deadline exceeded"))??;
        scope.settle().await;
        if scope.cancelled(){return Err(SpatialError::new("CANCELLED","Session open was cancelled"));}
        let Lease::Owned(client)=&mut lease else {unreachable!()};
        let handle=session::open_service(client.take().unwrap(),context.connection_id.clone(),context.read_only).await;
        scope.seal();scope.settle().await;
        if scope.cancelled() {
            if let Ok(mut slot)=session::lock_service(&handle,&context.connection_id,context.read_only).await {
                if let Some(client)=slot.client.take(){drop(deadpool_postgres::Object::take(client));}
            }
            session::forget_service(&handle).await;
            return Err(SpatialError::new("CANCELLED","Session open was cancelled"));
        }
        return Ok(json!({"session_handle":handle,"in_transaction":true,"rollback_confirmed":false}));
    }
    let handle=context.session_handle.as_deref().ok_or_else(||SpatialError::argument("session_handle is required"))?;
    let mut slot=tokio::time::timeout(std::time::Duration::from_millis(context.deadline_ms),session::lock_service(handle,&context.connection_id,context.read_only)).await
        .map_err(|_|SpatialError::new("DEADLINE_EXCEEDED","Session lock deadline exceeded"))??;
    if method=="service_session_discard" {
        types::input::<EmptyInput>(params)?;
        if let Some(client)=slot.client.take(){drop(deadpool_postgres::Object::take(client));}
        drop(slot);session::forget_service(handle).await;
        return Ok(json!({"session_handle":handle,"in_transaction":false,"rollback_confirmed":false}));
    }
    let input: FinishInput=types::input(params)?;
    if let Err(error)=verify_transaction(slot.client.as_ref().unwrap(),context.read_only).await {
        if let Some(client)=slot.client.take(){drop(deadpool_postgres::Object::take(client));}
        drop(slot);session::forget_service(handle).await;return Err(error);
    }
    let mut lease=Lease::Owned(Box::new(slot.client.take()));
    scope.attach(lease.client(),connection).await;
    let commit=input.action==Action::Commit;
    let outcome=lease.finish(&scope,commit,false).await;
    drop(slot);session::forget_service(handle).await;
    if let Err(mut error)=outcome {error.details=json!({"session_closed":true});return Err(error);}
    if scope.cancelled(){
        let mut error=SpatialError::new("CANCELLED","Session finish was cancelled; rollback was confirmed");
        error.details=json!({"session_closed":true});return Err(error);
    }
    Ok(json!({"session_handle":handle,"in_transaction":false,"rollback_confirmed":!commit}))
}
