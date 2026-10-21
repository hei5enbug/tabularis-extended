use tabularis_azure_launcher::{windows_quote::command_line, LaunchError, HEAP_ARGUMENT};

fn utf16(value: &str) -> Vec<u16> {
    value.encode_utf16().collect()
}

#[test]
fn 공백과_한글_경로는_각각_인용하고_고정_힙_인자를_보존한다() {
    // given
    let node = utf16(r"C:\공간 bundle\runtime\node.exe");
    let heap = utf16(HEAP_ARGUMENT);
    let entry = utf16(r"C:\공간 bundle\dist\driver.mjs");
    let expected = utf16("\"C:\\공간 bundle\\runtime\\node.exe\" \"--max-old-space-size=512\" \"C:\\공간 bundle\\dist\\driver.mjs\"\0");
    // when
    let result = command_line(&[&node, &heap, &entry]);
    // then
    assert_eq!(result, Ok(expected));
}

#[test]
fn 빈_인자와_끝의_백슬래시는_정확하게_인용한다() {
    // given
    let path = utf16(r"C:\space dir\");
    let expected = utf16("\"\" \"C:\\space dir\\\\\"\0");
    // when
    let result = command_line(&[&[], &path]);
    // then
    assert_eq!(result, Ok(expected));
}

#[test]
fn 따옴표_앞의_백슬래시는_윈도우_인자_규칙대로_이스케이프한다() {
    // given
    let input = utf16("a\\\"b");
    let expected = utf16("\"a\\\\\\\"b\"\0");
    // when
    let result = command_line(&[&input]);
    // then
    assert_eq!(result, Ok(expected));
}

#[test]
fn 널_문자가_있는_인자는_일반_오류로_거부한다() {
    // given
    let input = [b'a' as u16, 0, b'b' as u16];
    // when
    let result = command_line(&[&input]);
    // then
    assert_eq!(result, Err(LaunchError::InvalidCommandLine));
}

#[test]
fn 종료_널을_포함한_최대_명령줄_길이는_허용한다() {
    // given
    let input = vec![b'x' as u16; 32_764];
    // when
    let result = command_line(&[&input]);
    // then
    assert_eq!(result.as_ref().map(Vec::len), Ok(32_767));
    assert_eq!(result.as_ref().map(|value| value.last()), Ok(Some(&0)));
}

#[test]
fn 최대_길이를_한_코드유닛_넘은_명령줄은_거부한다() {
    // given
    let input = vec![b'x' as u16; 32_765];
    // when
    let result = command_line(&[&input]);
    // then
    assert_eq!(result, Err(LaunchError::InvalidCommandLine));
}

#[test]
fn 백슬래시_인용으로_길이가_늘어나면_상한을_다시_검사한다() {
    // given
    let input = vec![b'\\' as u16; 16_383];
    // when
    let result = command_line(&[&input]);
    // then
    assert_eq!(result, Err(LaunchError::InvalidCommandLine));
}

#[test]
fn 인자가_없는_명령줄은_거부한다() {
    // given
    let arguments: [&[u16]; 0] = [];
    // when
    let result = command_line(&arguments);
    // then
    assert_eq!(result, Err(LaunchError::InvalidCommandLine));
}

#[test]
fn 윈도우_경로의_유니코드_코드유닛은_변환하지_않는다() {
    // given
    let input = [0xd83d, 0xde80, 0xd800];
    // when
    let result = command_line(&[&input]);
    // then
    assert_eq!(result, Ok(vec![b'"' as u16, 0xd83d, 0xde80, 0xd800, b'"' as u16, 0]));
}
