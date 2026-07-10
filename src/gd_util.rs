//! Godot helpers shared by the editor plugin and the in-game runtime.
//! Everything here must be called on the Godot main thread.

use godot::classes::{GDScript, Node};
use godot::global::Error as GdError;
use godot::prelude::*;
use serde_json::{Value, json};

/// Serialize a node subtree to JSON: name, type, script, children.
pub fn serialize_node(node: &Gd<Node>, depth: i64) -> Value {
    let mut info = json!({
        "name": node.get_name().to_string(),
        "type": node.get_class().to_string(),
    });

    if let Some(script) = node.get_script() {
        let path = script.get_path().to_string();
        if !path.is_empty() {
            info["script"] = json!(path);
        }
    }

    if depth > 0 && node.get_child_count() > 0 {
        let children: Vec<Value> = node
            .get_children()
            .iter_shared()
            .map(|child| serialize_node(&child, depth - 1))
            .collect();
        info["children"] = json!(children);
    }

    info
}

/// Run arbitrary GDScript: the code becomes the body of `func run():` in a
/// RefCounted script. The `@tool` annotation makes it work in the editor too;
/// it is harmless at game runtime. The return value is serialized to JSON.
pub fn run_gdscript(code: &str) -> Result<Value, String> {
    let mut body = String::new();
    for line in code.lines() {
        body.push('\t');
        body.push_str(line);
        body.push('\n');
    }
    let source = format!("@tool\nextends RefCounted\nfunc run():\n{body}");

    let mut script = GDScript::new_gd();
    script.set_source_code(&source);
    let err = script.reload();
    if err != GdError::OK {
        return Err(format!(
            "GDScript parse error ({err:?}). The code runs inside `func run():` — check indentation and syntax."
        ));
    }

    let instance = script.call("new", &[]);
    let mut instance = instance
        .try_to::<Gd<RefCounted>>()
        .map_err(|e| format!("Failed to instantiate script: {e}"))?;
    let result = instance.call("run", &[]);
    Ok(variant_to_json(&result, 0))
}

pub fn variant_to_json(value: &Variant, depth: u32) -> Value {
    if depth > 8 {
        return json!(value.to_string());
    }
    match value.get_type() {
        VariantType::NIL => Value::Null,
        VariantType::BOOL => json!(value.to::<bool>()),
        VariantType::INT => json!(value.to::<i64>()),
        VariantType::FLOAT => json!(value.to::<f64>()),
        VariantType::STRING | VariantType::STRING_NAME | VariantType::NODE_PATH => {
            json!(value.to_string())
        }
        VariantType::ARRAY => {
            let arr = value.to::<VarArray>();
            Value::Array(
                arr.iter_shared()
                    .map(|v| variant_to_json(&v, depth + 1))
                    .collect(),
            )
        }
        VariantType::DICTIONARY => {
            let dict = value.to::<Dictionary<Variant, Variant>>();
            let mut map = serde_json::Map::new();
            for (k, v) in dict.iter_shared() {
                map.insert(k.to_string(), variant_to_json(&v, depth + 1));
            }
            Value::Object(map)
        }
        VariantType::PACKED_STRING_ARRAY => {
            let arr = value.to::<PackedStringArray>();
            Value::Array(arr.to_vec().iter().map(|s| json!(s.to_string())).collect())
        }
        _ => json!(value.to_string()),
    }
}
