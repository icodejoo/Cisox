//! 往返与校验测试：真实样本 + 合成边界样本。
use super::*;
use serde_json::json;

/// 真实样本 index.json（复制自参照版）。
const REAL: &str = include_str!("../tests/fixtures/sample/index.json");

/// 断言"读入→再序列化"与原文语义一致。
fn assert_roundtrip(text: &str) -> HistoryIndex {
    let index = load_index(text.as_bytes()).unwrap();
    let out = save_index(&index).unwrap();
    let a: Value = serde_json::from_str(text).unwrap();
    let b: Value = serde_json::from_slice(&out).unwrap();
    assert!(json_semantic_eq(&a, &b), "往返不等价:\n{a}\n{b}");
    index
}

/// 真实样本往返、字段值与磁盘校验。
#[test]
fn real_sample_roundtrip_and_fields() {
    let idx = assert_roundtrip(REAL);
    assert_eq!(idx.format_version, 2);
    assert!(idx.pending_deletions.is_empty());
    let r = &idx.records[0];
    assert_eq!(r.displays.len(), 2);
    assert_eq!(r.displays[1].image_file, "display_1.png");
    assert_eq!(r.displays[0].display_name, "\\\\.\\DISPLAY3");
    assert_eq!(r.scrolling, Some(false));
    let g = r.desktop_geometry.as_ref().unwrap();
    assert_eq!((g.space.as_str(), g.x, g.y), ("pixels", 0, 0));
    assert_eq!(r.selection.shadow_color, "#FF333333");
    assert!(r.selection.geometry.is_none() && r.selection.regions.is_none());
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample");
    validate_index_on_disk(&idx, &root).unwrap();
}

/// 真实样本字节级一致（键序均为字母序、紧凑格式）。
#[test]
fn real_sample_byte_identical() {
    let idx = load_index(REAL.as_bytes()).unwrap();
    assert_eq!(
        String::from_utf8(save_index(&idx).unwrap()).unwrap(),
        REAL.trim_end()
    );
}

/// 真实样本 canvas 为合法 JSON，schemaVersion=5，长度与 canvas_byte_size 一致。
#[test]
fn real_canvas_is_opaque_json() {
    let raw = include_bytes!(
        "../tests/fixtures/sample/records/b5ac6a4d-8770-4b99-b83c-414a3ef55710/canvas_history.json"
    );
    let v: Value = serde_json::from_slice(raw).unwrap();
    assert_eq!(v["schemaVersion"], 5);
    assert_eq!(
        raw.len() as i64,
        load_index(REAL.as_bytes()).unwrap().records[0].canvas_byte_size
    );
}

/// 合成边界样本：v1、pending 非空、image 类型、含 source_canvas_rect、regions、各层未知字段。
fn synthetic() -> String {
    json!({
        "format_version": 1,
        "future_root": {"a": [1, 2.5, null]},
        "pending_deletions": [{"id": "11111111-2222-3333-4444-555555555555", "bytes": 42, "note": "x"}],
        "records": [{
            "id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "created_utc": "2026-01-02T03:04:05.006Z",
            "source": "pinned_to_screen",
            "content_kind": "image",
            "canvas_bounds": {"x": -1920, "y": -10, "width": 3840, "height": 1080, "z": 1},
            "selection": {
                "rectangle": {"x": 0, "y": 0, "width": 10, "height": 10},
                "corner_radius": 4, "shadow_width": 2, "shadow_color": "#80112233",
                "lock_aspect_ratio": true, "lock_drag_aspect_ratio": false,
                "regions": [{"x": 0, "y": 0, "width": 5, "height": 5}],
                "future_sel": "keep"
            },
            "canvas_history_file": "canvas_history.json",
            "canvas_byte_size": 10, "total_record_size": 40,
            "displays": [{
                "image_file": "display_0.png", "width": 100, "height": 50, "encoded_bytes": 20,
                "stable_id": "s", "display_name": "d",
                "source_canvas_origin": {"x": -1920, "y": 0},
                "source_canvas_rect": {"x": -1920, "y": 0, "width": 50, "height": 25},
                "backing_scale": 2.0, "native_display_id": 4294967295u32, "canvas_space": "points",
                "future_d": {"k": 1}
            }],
            "result": {"image_file": "capture_result.png", "width": 5, "height": 5, "encoded_bytes": 10, "r_extra": true},
            "scrolling": true,
            "desktop_geometry": {"space": "points", "x": -1920, "y": 0},
            "future_rec": [1, 2, 3]
        }]
    })
    .to_string()
}

/// 合成样本往返，且未知字段在每一层都保留。
#[test]
fn synthetic_roundtrip_keeps_unknown_fields() {
    let idx = assert_roundtrip(&synthetic());
    assert_eq!(idx.extra["future_root"], json!({"a": [1, 2.5, null]}));
    assert_eq!(idx.pending_deletions[0].extra["note"], "x");
    let r = &idx.records[0];
    assert_eq!(r.extra["future_rec"], json!([1, 2, 3]));
    assert_eq!(r.canvas_bounds.extra["z"], 1);
    assert_eq!(r.selection.extra["future_sel"], "keep");
    assert_eq!(r.displays[0].extra["future_d"], json!({"k": 1}));
    assert_eq!(r.result.as_ref().unwrap().extra["r_extra"], true);
    assert_eq!(r.displays[0].native_display_id, Some(4294967295));
    assert_eq!(r.displays[0].canvas_space.as_deref(), Some("points"));
    validate_record(r).unwrap();
}

/// 缺省的可选字段在输出中不应凭空出现（旧记录无 scrolling/desktop_geometry 等）。
#[test]
fn absent_optionals_stay_absent() {
    let text = r#"{"format_version":2,"pending_deletions":[],"records":[]}"#;
    let idx = assert_roundtrip(text);
    assert_eq!(save_index(&idx).unwrap(), text.as_bytes());
    let mut v: Value = serde_json::from_str(&synthetic()).unwrap();
    let rec = v["records"][0].as_object_mut().unwrap();
    for k in ["scrolling", "desktop_geometry", "content_kind", "result"] {
        rec.remove(k);
    }
    let out = save_index(&load_index(v.to_string().as_bytes()).unwrap()).unwrap();
    let o: Value = serde_json::from_slice(&out).unwrap();
    assert!(o["records"][0].get("scrolling").is_none() && o["records"][0].get("result").is_none());
}

/// 版本 3 及以上被拒绝，与 C++ 一致。
#[test]
fn unsupported_version_rejected() {
    let text = r#"{"format_version":3,"pending_deletions":[],"records":[]}"#;
    assert!(load_index(text.as_bytes()).is_err());
}

/// 生成根级未知字段嵌套 `depth` 层数组的索引文本。
fn index_with_nested_unknown(depth: usize) -> String {
    format!(
        r#"{{"format_version":2,"pending_deletions":[],"records":[],"future":{}{}{}}}"#,
        "[".repeat(depth),
        "0",
        "]".repeat(depth)
    )
}

/// 未知字段嵌套在 serde_json 深度上限（128）内可往返；超限整份索引作废但不 panic。
///
/// 注：C++ `QJsonDocument` 上限为 1024，此处差异属已知现状（见交接文档 §3.1）。
#[test]
fn nested_unknown_field_depth_limit() {
    let ok = load_index(index_with_nested_unknown(100).as_bytes()).unwrap();
    assert!(ok.extra.contains_key("future"));
    assert!(load_index(index_with_nested_unknown(200).as_bytes()).is_err());
}

/// 缺少 pending_deletions 应解析失败，与 C++ 的 isArray 检查一致。
#[test]
fn missing_pending_deletions_rejected() {
    assert!(load_index(br#"{"format_version":2,"records":[]}"#).is_err());
}

/// 校验规则：大小合计、文件命名、非法 source、非法 UUID、非法 content_kind。
#[test]
fn validation_rules() {
    let good = load_index(synthetic().as_bytes())
        .unwrap()
        .records
        .remove(0);
    validate_record(&good).unwrap();
    let mut r = good.clone();
    r.total_record_size += 1;
    assert!(validate_record(&r).is_err());
    let mut r = good.clone();
    r.displays[0].image_file = "display_1.png".into();
    assert!(validate_record(&r).is_err());
    let mut r = good.clone();
    r.source = "nope".into();
    assert!(validate_record(&r).is_err());
    let mut r = good.clone();
    r.id = "{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}".into();
    assert!(validate_record(&r).is_err());
    let mut r = good;
    r.content_kind = Some("video".into());
    assert!(validate_record(&r).is_err());
}

/// 数字写法差异（1 与 1.0）语义比较视为相等，值或键集合不同仍被识别。
#[test]
fn semantic_eq_number_forms() {
    assert!(json_semantic_eq(
        &json!({"a": 1}),
        &serde_json::from_str("{\"a\":1.0}").unwrap()
    ));
    assert!(!json_semantic_eq(&json!({"a": 1}), &json!({"a": 2})));
    assert!(!json_semantic_eq(
        &json!({"a": 1}),
        &json!({"a": 1, "b": 1})
    ));
}

/// 小写 UUID 判定。
#[test]
fn uuid_check() {
    assert!(is_valid_uuid("b5ac6a4d-8770-4b99-b83c-414a3ef55710"));
    assert!(!is_valid_uuid("B5AC6A4D-8770-4B99-B83C-414A3EF55710"));
    assert!(!is_valid_uuid("xyz"));
    assert!(!is_valid_uuid("00000000-0000-0000-0000-000000000000"));
}

/// Geometry 自定义形状边界 (合成)
#[test]
fn geometry_validation_rules() {
    let v = json!({
        "version": 1,
        "rectangles": [
            [0, 0, 10, 10],
            [1.0, 2.0, 10.0, 10.0]
        ]
    });
    let g: RegionGeometry = serde_json::from_value(v.clone()).unwrap();
    validate_geometry(&g).unwrap();

    // rectangles
    let mut bad = v.clone();
    bad["rectangles"][0] = json!([0, 0, 10, 10.5]);
    assert!(
        validate_geometry(&serde_json::from_value(bad).unwrap())
            .unwrap_err()
            .contains("整数")
    );

    let mut bad = v.clone();
    bad["rectangles"][0] = json!([0, 0, 10]);
    assert!(
        validate_geometry(&serde_json::from_value(bad).unwrap())
            .unwrap_err()
            .contains("长度")
    );

    let mut bad = v.clone();
    bad["rectangles"][0] = json!([0, 0, 0, 10]);
    assert!(
        validate_geometry(&serde_json::from_value(bad).unwrap())
            .unwrap_err()
            .contains("宽或高 < 1")
    );

    let mut bad = v.clone();
    bad["rectangles"][0] = json!([10000001, 0, 10, 10]);
    assert!(
        validate_geometry(&serde_json::from_value(bad).unwrap())
            .unwrap_err()
            .contains("越界")
    );

    let mut bad = v.clone();
    bad["version"] = json!(2);
    assert!(
        validate_geometry(&serde_json::from_value(bad).unwrap())
            .unwrap_err()
            .contains("version")
    );

    let mut bad = v.clone();
    bad.as_object_mut().unwrap().remove("version");
    // 缺 version：untagged 回退为 Unknown 变体，校验必须拒绝
    assert!(
        validate_geometry(&serde_json::from_value(bad).unwrap())
            .unwrap_err()
            .contains("未知")
    );

    let mut bad = v.clone();
    bad["operands"] = json!([]);
    assert!(
        validate_geometry(&serde_json::from_value(bad).unwrap())
            .unwrap_err()
            .contains("互斥")
    );

    // operands
    let v2 = json!({
        "version": 1,
        "operands": [
            {
                "operation": 0,
                "type": "rectangle",
                "fill": 1,
                "commands": [
                    [0, 0.0, 0.0],
                    [1, 10.0, 0.0],
                    [2, 20.0, 10.0], [3, 20.0, 20.0], [3, 10.0, 20.0]
                ],
                "unknown": 42
            }
        ]
    });
    let g2: RegionGeometry = serde_json::from_value(v2.clone()).unwrap();
    validate_geometry(&g2).unwrap();

    let mut bad = v2.clone();
    bad["operands"][0]["operation"] = json!(1);
    assert!(
        validate_geometry(&serde_json::from_value(bad).unwrap())
            .unwrap_err()
            .contains("operation 必须为 0")
    );

    let mut bad = v2.clone();
    bad["operands"].as_array_mut().unwrap().push(json!({
        "operation": 3,
        "type": "rectangle",
        "fill": 1,
        "commands": [[0, 0.0, 0.0]]
    }));
    assert!(
        validate_geometry(&serde_json::from_value(bad).unwrap())
            .unwrap_err()
            .contains("operation 非法")
    );

    let mut bad = v2.clone();
    bad["operands"][0]["fill"] = json!(2);
    assert!(
        validate_geometry(&serde_json::from_value(bad).unwrap())
            .unwrap_err()
            .contains("fill")
    );

    let mut bad = v2.clone();
    bad["operands"][0]["type"] = json!("star");
    assert!(
        validate_geometry(&serde_json::from_value(bad).unwrap())
            .unwrap_err()
            .contains("未知")
    );

    let mut bad = v2.clone();
    bad["operands"][0]["commands"] = json!([]);
    assert!(
        validate_geometry(&serde_json::from_value(bad).unwrap())
            .unwrap_err()
            .contains("commands")
    );

    let mut bad = v2.clone();
    bad["operands"][0]["commands"][0][0] = json!(1);
    assert!(
        validate_geometry(&serde_json::from_value(bad).unwrap())
            .unwrap_err()
            .contains("moveTo (0)")
    );

    let mut bad = v2.clone();
    bad["operands"][0]["commands"]
        .as_array_mut()
        .unwrap()
        .remove(3);
    assert!(
        validate_geometry(&serde_json::from_value(bad).unwrap())
            .unwrap_err()
            .contains("控制点")
    );

    let mut bad = v2.clone();
    bad["operands"][0]["commands"][0][1] = json!(10000001);
    assert!(
        validate_geometry(&serde_json::from_value(bad).unwrap())
            .unwrap_err()
            .contains("不合法或越界")
    );

    let mut bad = v2.clone();
    bad["operands"] = json!([]);
    assert!(
        validate_geometry(&serde_json::from_value(bad).unwrap())
            .unwrap_err()
            .contains("为空")
    );
}

/// Selection 的边界 (合成)
#[test]
fn selection_validation_rules() {
    let rec = load_index(synthetic().as_bytes())
        .unwrap()
        .records
        .remove(0);

    let mut bad = rec.clone();
    bad.selection.corner_radius = 257;
    assert!(validate_record(&bad).unwrap_err().contains("corner_radius"));

    let mut bad = rec.clone();
    bad.selection.shadow_width = 65;
    assert!(validate_record(&bad).unwrap_err().contains("shadow_width"));

    let mut bad = rec.clone();
    bad.selection.shadow_color = "red".to_string();
    assert!(validate_record(&bad).unwrap_err().contains("shadow_color"));

    let mut bad = rec.clone();
    bad.selection.rectangle.width = 0;
    assert!(validate_record(&bad).unwrap_err().contains("w/h"));

    let mut bad = rec.clone();
    bad.selection.regions = Some(json!([]));
    assert!(validate_record(&bad).unwrap_err().contains("regions 数量"));

    let mut bad = rec.clone();
    bad.selection.regions = Some(json!([
        {"x": -1000000, "y": 0, "width": 10, "height": 10},
        {"x": 2147000000, "y": 0, "width": 10, "height": 10}
    ]));
    assert!(validate_record(&bad).unwrap_err().contains("并集"));
}

/// Record 的范围及逻辑校验
#[test]
fn record_range_validation() {
    let rec = load_index(synthetic().as_bytes())
        .unwrap()
        .records
        .remove(0);

    let mut bad = rec.clone();
    bad.canvas_byte_size = 0;
    assert!(
        validate_record(&bad)
            .unwrap_err()
            .contains("canvas_byte_size")
    );

    let mut bad = rec.clone();
    bad.canvas_byte_size = MAX_CANVAS_BYTES + 1;
    assert!(
        validate_record(&bad)
            .unwrap_err()
            .contains("canvas_byte_size")
    );

    let mut bad = rec.clone();
    bad.total_record_size = MAX_STORED_BYTES + 1;
    assert!(
        validate_record(&bad)
            .unwrap_err()
            .contains("total_record_size")
    );

    let mut bad = rec.clone();
    bad.canvas_bounds.width = 0;
    assert!(validate_record(&bad).unwrap_err().contains("canvas_bounds"));

    let mut bad = rec.clone();
    bad.canvas_bounds.y = 10;
    bad.canvas_bounds.height = INT_MAX;
    assert!(validate_record(&bad).unwrap_err().contains("INT_MAX"));

    let mut bad = rec.clone();
    bad.desktop_geometry.as_mut().unwrap().x = INT_MAX + 1;
    assert!(
        validate_record(&bad)
            .unwrap_err()
            .contains("desktop_geometry x/y 越界")
    );

    let mut bad = rec.clone();
    bad.displays[0].encoded_bytes = 0;
    assert!(validate_record(&bad).unwrap_err().contains("encoded_bytes"));

    let mut bad = rec.clone();
    bad.displays[0].width = 8000;
    bad.displays[0].height = 8001; // 64,008,000 > MAX_PIXELS_PER_IMAGE
    assert!(validate_record(&bad).unwrap_err().contains("单张"));

    let mut bad = rec.clone();
    bad.result = Some(ResultImage {
        image_file: "capture_result.png".to_string(),
        width: 8000,
        height: 8000,
        encoded_bytes: 10,
        extra: Extra::new(),
    });
    bad.displays.push(Display {
        image_file: "display_1.png".to_string(),
        width: 8000,
        height: 8000,
        encoded_bytes: 10,
        stable_id: "s2".to_string(),
        display_name: "d2".to_string(),
        source_canvas_origin: None,
        source_canvas_rect: None,
        backing_scale: None,
        native_display_id: None,
        canvas_space: None,
        extra: Extra::new(),
    });
    bad.total_record_size = bad.canvas_byte_size + bad.displays[0].encoded_bytes + 20; // sync sizes
    assert!(validate_record(&bad).unwrap_err().contains("总像素"));

    let mut bad = rec.clone();
    bad.displays[0].source_canvas_origin.as_mut().unwrap().x = INT_MIN - 1;
    assert!(validate_record(&bad).unwrap_err().contains("origin"));

    let mut bad = rec.clone();
    bad.displays[0].source_canvas_rect.as_mut().unwrap().width = 0;
    assert!(
        validate_record(&bad)
            .unwrap_err()
            .contains("source_canvas_rect w/h")
    );

    let mut bad = rec.clone();
    bad.displays[0].backing_scale = Some(0.0);
    assert!(validate_record(&bad).unwrap_err().contains("backing_scale"));

    let mut bad = rec.clone();
    bad.displays[0].backing_scale = Some(-1.0);
    assert!(validate_record(&bad).unwrap_err().contains("backing_scale"));

    let mut bad = rec.clone();
    bad.displays[0].canvas_space = None;
    assert!(validate_record(&bad).unwrap_err().contains("canvas_space"));

    let mut bad = rec.clone();
    bad.displays[0].native_display_id = Some(-1);
    assert!(
        validate_record(&bad)
            .unwrap_err()
            .contains("native_display_id 越界")
    );

    let mut bad = rec.clone();
    bad.displays[0].native_display_id = Some(4294967296);
    assert!(
        validate_record(&bad)
            .unwrap_err()
            .contains("native_display_id 越界")
    );
}

/// Index 及 pending_deletions 和 size 的逻辑校验
#[test]
fn index_validation_rules() {
    let idx = load_index(synthetic().as_bytes()).unwrap();

    let mut bad = idx.clone();
    bad.records.push(bad.records[0].clone());
    assert!(validate_index(&bad).unwrap_err().contains("ID 重复"));

    let mut bad = idx.clone();
    bad.pending_deletions[0].id = bad.records[0].id.clone();
    assert!(validate_index(&bad).unwrap_err().contains("ID 冲突"));

    let mut bad = idx.clone();
    bad.pending_deletions[0].id = "not-a-uuid".to_string();
    assert!(validate_index(&bad).unwrap_err().contains("不合法"));

    let mut bad = idx.clone();
    bad.pending_deletions[0].bytes = -1;
    assert!(
        validate_index(&bad)
            .unwrap_err()
            .contains("pending_deletions bytes 越界")
    );

    let mut bad = idx.clone();
    bad.pending_deletions[0].bytes = MAX_STORED_BYTES + 1;
    assert!(
        validate_index(&bad)
            .unwrap_err()
            .contains("pending_deletions bytes 越界")
    );

    assert!(check_total_bytes(MAX_STORED_BYTES, 0).is_ok());
    assert!(check_total_bytes(MAX_STORED_BYTES, 1).is_err());
    assert!(check_total_bytes(MAX_STORED_BYTES - 100, 100).is_ok());
    assert!(check_total_bytes(MAX_STORED_BYTES - 100, 101).is_err());
}
