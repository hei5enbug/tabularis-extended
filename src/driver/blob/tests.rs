use super::*;

#[test]
fn wire_format_contains_size_mime_and_base64() {
    let bytes = [0xCA, 0xFE, 0xBA, 0xBE];
    assert_eq!(
        encode_blob_full(&bytes, 4).unwrap(),
        "BLOB:4:application/octet-stream:yv66vg=="
    );
}

#[test]
fn wire_format_decodes_for_row_editing_and_checks_declared_size() {
    assert_eq!(
        decode_blob_wire("BLOB:4:application/octet-stream:yv66vg==").unwrap(),
        Some(vec![0xCA, 0xFE, 0xBA, 0xBE])
    );
    assert_eq!(decode_blob_wire("ordinary text").unwrap(), None);
    assert!(decode_blob_wire("BLOB:3:application/octet-stream:yv66vg==")
        .unwrap_err()
        .contains("declares 3 bytes"));
}

#[test]
fn wire_format_sniffs_png_magic_bytes() {
    let png_signature = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let wire = encode_blob_full(&png_signature, 8).unwrap();
    assert!(wire.starts_with("BLOB:8:image/png:"));
}

#[test]
fn preview_size_ceiling_accepts_exact_limit_and_rejects_larger_value() {
    assert!(encode_blob_full(&[1, 2, 3, 4], 4).is_ok());

    let error = encode_blob_full(&[1, 2, 3, 4], 3).unwrap_err();
    assert!(error.contains("4 bytes"));
    assert!(error.contains("max_blob_size of 3 bytes"));
}

#[test]
fn binary_varbinary_and_image_are_user_blob_types() {
    for data_type in ["binary(8)", "VARBINARY(MAX)", "image"] {
        assert!(
            validate_blob_data_type(data_type).is_ok(),
            "rejected {data_type}"
        );
    }
}

#[test]
fn rowversion_and_timestamp_are_not_offered_as_blobs() {
    for data_type in ["ROWVERSION", "timestamp"] {
        let error = validate_blob_data_type(data_type).unwrap_err();
        assert!(error.contains("concurrency token"));
    }
}

/// 빈 출력 경로는 거부한다.
#[test]
fn writable_path_validation_rejects_empty_directory_and_missing_parent() {
    // given
    let path = "";

    // when
    let result = validate_writable_file_path(path);

    // then
    assert!(result.is_err());
}

/// 출력 경로가 기존 디렉터리이면 거부한다.
#[test]
fn writable_path_validation_rejects_existing_directory() {
    // given
    let directory = std::env::temp_dir();

    // when
    let result = validate_writable_file_path(&directory.to_string_lossy());

    // then
    assert!(result.is_err());
}

/// 출력 파일의 상위 디렉터리가 없으면 거부한다.
#[test]
fn writable_path_validation_rejects_missing_parent() {
    // given
    let path = std::env::temp_dir()
        .join(uuid::Uuid::new_v4().to_string())
        .join("out.bin");

    // when
    let result = validate_writable_file_path(&path.to_string_lossy());

    // then
    assert!(result.is_err());
}

/// 기존 상위 디렉터리의 출력 파일 경로는 허용한다.
#[test]
fn writable_path_validation_accepts_existing_parent_and_bare_filename() {
    // given
    let path = std::env::temp_dir().join("ss012-output.bin");

    // when
    let result = validate_writable_file_path(&path.to_string_lossy());

    // then
    assert!(result.is_ok());
}

/// 디렉터리 없이 지정한 출력 파일 이름은 허용한다.
#[test]
fn writable_path_validation_accepts_bare_filename() {
    // given
    let path = "ss012-output.bin";

    // when
    let result = validate_writable_file_path(path);

    // then
    assert!(result.is_ok());
}
