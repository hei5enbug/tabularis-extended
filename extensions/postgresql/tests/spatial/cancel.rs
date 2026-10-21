use super::*;

#[tokio::test]
async fn 인증_무효화는_해당_연결의_모든_요청만_취소한다() {
    // given
    let connection = uuid::Uuid::new_v4().to_string();
    let first = ScopedGuard::reserve(&connection,"first").unwrap();
    let second = ScopedGuard::reserve(&connection,"second").unwrap();
    let other = ScopedGuard::reserve("other-connection",&uuid::Uuid::new_v4().to_string()).unwrap();
    // when
    let count = cancel_connection(&connection).await;
    // then
    assert_eq!(count,2);
    assert!(first.cancelled());
    assert!(second.cancelled());
    assert!(!other.cancelled());
}

#[tokio::test]
async fn 같은_요청_id의_다른_연결을_취소하지_않는다() {
    // given
    let request=uuid::Uuid::new_v4().to_string();
    let first=ScopedGuard::reserve("first",&request).unwrap();
    let second=ScopedGuard::reserve("second",&request).unwrap();
    // when
    let accepted=cancel_scoped("first",&request).await;
    // then
    assert!(accepted);
    assert!(first.cancelled());
    assert!(!second.cancelled());
}

#[test]
fn 진행_중인_연결의_요청_id를_중복_예약하지_않는다() {
    // given
    let request=uuid::Uuid::new_v4().to_string();
    let _first=ScopedGuard::reserve("duplicate",&request).unwrap();
    // when
    let result=ScopedGuard::reserve("duplicate",&request);
    // then
    assert!(result.is_err());
}

#[tokio::test]
async fn 완료한_요청의_취소는_새_작업에_전달하지_않는다() {
    // given
    let request=uuid::Uuid::new_v4().to_string();
    let scope=ScopedGuard::reserve("finished",&request).unwrap();
    scope.seal();
    // when
    let accepted=cancel_scoped("finished",&request).await;
    // then
    assert!(!accepted);
    assert!(!scope.cancelled());
}
