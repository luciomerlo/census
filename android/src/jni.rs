//! Puente JNI hacia `com.censusindex.android.Native`. Todo devuelve un `String`
//! JSON (`{"error": "..."}` si falla) para que Kotlin no maneje excepciones
//! nativas. El `handle` es un `Arc<Engine>` entregado por `nativeOpen`.

use std::path::Path;
use std::sync::Arc;

use jni::objects::{JClass, JString};
use jni::sys::{jboolean, jint, jlong, jstring};
use jni::JNIEnv;
use serde_json::{json, Value};

use crate::engine::Engine;

fn s(env: &mut JNIEnv, v: &JString) -> String {
    env.get_string(v).map(|x| x.into()).unwrap_or_default()
}

fn out(env: &mut JNIEnv, v: Value) -> jstring {
    env.new_string(v.to_string())
        .map(|j| j.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

fn res(env: &mut JNIEnv, r: Result<Value, String>) -> jstring {
    out(env, r.unwrap_or_else(|e| json!({"error": e})))
}

/// # Safety
/// `h` debe venir de `nativeOpen` y no haberse liberado.
unsafe fn eng(h: jlong) -> Option<&'static Arc<Engine>> {
    (h as *const Arc<Engine>).as_ref()
}

#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativeOpen(
    mut env: JNIEnv,
    _c: JClass,
    dir: JString,
    pc_id: JString,
    device: JString,
) -> jlong {
    let (d, p, dv) = (s(&mut env, &dir), s(&mut env, &pc_id), s(&mut env, &device));
    Box::into_raw(Box::new(Arc::new(Engine::open(Path::new(&d), &p, &dv)))) as jlong
}

#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativeSetVolumes(
    mut env: JNIEnv,
    _c: JClass,
    h: jlong,
    volumes: JString,
) -> jstring {
    let v = s(&mut env, &volumes);
    let r = unsafe { eng(h) }
        .ok_or_else(|| "handle invalido".to_string())
        .and_then(|e| e.set_volumes(&v))
        .map(|n| json!({"volumes": n}));
    res(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativeFull(
    mut env: JNIEnv,
    _c: JClass,
    h: jlong,
) -> jstring {
    let r = unsafe { eng(h) }
        .ok_or_else(|| "handle invalido".to_string())
        .and_then(|e| e.full());
    res(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativeSweep(
    mut env: JNIEnv,
    _c: JClass,
    h: jlong,
) -> jstring {
    let r = unsafe { eng(h) }
        .ok_or_else(|| "handle invalido".to_string())
        .and_then(|e| e.sweep());
    res(&mut env, r)
}

/// `dirs_json`: arreglo JSON de rutas absolutas de carpetas sucias.
#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativeDirty(
    mut env: JNIEnv,
    _c: JClass,
    h: jlong,
    dirs_json: JString,
) -> jstring {
    let text = s(&mut env, &dirs_json);
    let r = serde_json::from_str::<Vec<String>>(&text)
        .map_err(|e| format!("dirs: {e}"))
        .and_then(|dirs| {
            unsafe { eng(h) }
                .ok_or_else(|| "handle invalido".to_string())
                .and_then(|e| e.dirty(dirs))
        });
    res(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativeSearch(
    mut env: JNIEnv,
    _c: JClass,
    h: jlong,
    q: JString,
    limit: jint,
) -> jstring {
    let q = s(&mut env, &q);
    let r = unsafe { eng(h) }
        .map(|e| e.search(&q, limit.max(0) as usize))
        .ok_or_else(|| "handle invalido".to_string());
    res(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativeStats(
    mut env: JNIEnv,
    _c: JClass,
    h: jlong,
) -> jstring {
    let r = unsafe { eng(h) }
        .map(|e| e.stats())
        .ok_or_else(|| "handle invalido".to_string());
    res(&mut env, r)
}

/// Bloquea hasta que el servidor muera: Kotlin lo llama desde un hilo propio.
#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativeServe(
    mut env: JNIEnv,
    _c: JClass,
    h: jlong,
    port: jint,
    lan: jboolean,
    token: JString,
) -> jstring {
    let token = s(&mut env, &token);
    let r = unsafe { eng(h) }
        .ok_or_else(|| "handle invalido".to_string())
        .and_then(|e| e.serve(port as u16, lan != 0, &token))
        .map(|_| json!({"ok": true}));
    res(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativeClose(
    _e: JNIEnv,
    _c: JClass,
    h: jlong,
) {
    if h != 0 {
        drop(unsafe { Box::from_raw(h as *mut Arc<Engine>) });
    }
}

fn with<F: FnOnce(&Arc<Engine>) -> Result<Value, String>>(h: jlong, f: F) -> Result<Value, String> {
    unsafe { eng(h) }
        .ok_or_else(|| "handle invalido".to_string())
        .and_then(f)
}

#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativeDeviceInfo(
    mut env: JNIEnv,
    _c: JClass,
    h: jlong,
) -> jstring {
    let r = with(h, |e| Ok(e.device_info()));
    res(&mut env, r)
}

/// `""` vuelve al nombre del sistema.
#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativeSetDeviceName(
    mut env: JNIEnv,
    _c: JClass,
    h: jlong,
    name: JString,
) -> jstring {
    let n = s(&mut env, &name);
    let r = with(h, |e| e.set_device_name(&n));
    res(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativeAiGet(
    mut env: JNIEnv,
    _c: JClass,
    h: jlong,
) -> jstring {
    let r = with(h, |e| Ok(e.ai_get()));
    res(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativeAiSet(
    mut env: JNIEnv,
    _c: JClass,
    h: jlong,
    cfg: JString,
) -> jstring {
    let c = s(&mut env, &cfg);
    let r = with(h, |e| e.ai_set(&c));
    res(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativeAiModels(
    mut env: JNIEnv,
    _c: JClass,
    h: jlong,
    endpoint: JString,
) -> jstring {
    let c = s(&mut env, &endpoint);
    let r = with(h, |e| e.ai_models(&c));
    res(&mut env, r)
}

/// Bloquea por red: Kotlin lo llama desde un hilo de fondo.
#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativeAsk(
    mut env: JNIEnv,
    _c: JClass,
    h: jlong,
    question: JString,
    spanish: jboolean,
) -> jstring {
    let q = s(&mut env, &question);
    let r = with(h, |e| e.ask(&q, spanish != 0));
    res(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativeVolumesUpdate(
    mut env: JNIEnv,
    _c: JClass,
    h: jlong,
    volumes: JString,
) -> jstring {
    let v = s(&mut env, &volumes);
    let r = with(h, |e| e.volumes_update(&v));
    res(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativeSetPolicy(
    mut env: JNIEnv,
    _c: JClass,
    h: jlong,
    volume_id: JString,
    policy: JString,
) -> jstring {
    let (v, p) = (s(&mut env, &volume_id), s(&mut env, &policy));
    let r = with(h, |e| e.set_policy(&v, &p));
    res(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativePolicyGet(
    mut env: JNIEnv,
    _c: JClass,
    h: jlong,
) -> jstring {
    let r = with(h, |e| Ok(e.policy_get()));
    res(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativeIndexOnce(
    mut env: JNIEnv,
    _c: JClass,
    h: jlong,
    volume_id: JString,
) -> jstring {
    let v = s(&mut env, &volume_id);
    let r = with(h, |e| e.index_once(&v));
    res(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_com_censusindex_android_Native_nativeIndexPending(
    mut env: JNIEnv,
    _c: JClass,
    h: jlong,
) -> jstring {
    let r = with(h, |e| e.index_pending());
    res(&mut env, r)
}
