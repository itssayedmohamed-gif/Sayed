use android_activity::AndroidApp;
use jni::errors::Result as JResult;
use jni::objects::{JObject, JString, JValue};
use jni::{JNIEnv, JavaVM};
use std::sync::OnceLock;

static CTX: OnceLock<(usize, usize)> = OnceLock::new();

pub fn init(app: &AndroidApp) {
    let _ = CTX.set((app.vm_as_ptr() as usize, app.activity_as_ptr() as usize));
}

fn with_env<R>(f: impl FnOnce(&mut JNIEnv, &JObject) -> JResult<R>) -> Option<R> {
    let (vm_ptr, act_ptr) = *CTX.get()?;
    let vm = unsafe { JavaVM::from_raw(vm_ptr as *mut jni::sys::JavaVM) }.ok()?;
    let mut guard = vm.attach_current_thread().ok()?;
    let act = unsafe { JObject::from_raw(act_ptr as jni::sys::jobject) };
    let result = f(&mut *guard, &act);
    if guard.exception_check().unwrap_or(false) {
        let _ = guard.exception_clear();
    }
    result.ok()
}

/// true لو التطبيق واخد صلاحية "All files access"
pub fn has_all_files_access() -> bool {
    with_env(|env, _| {
        env.call_static_method(
            "android/os/Environment",
            "isExternalStorageManager",
            "()Z",
            &[],
        )?
        .z()
    })
    .unwrap_or(false)
}

fn open_settings(
    env: &mut JNIEnv,
    act: &JObject,
    pkg: &str,
    action: &str,
    with_pkg: bool,
) -> JResult<()> {
    let a = env.new_string(action)?;
    let intent = env.new_object(
        "android/content/Intent",
        "(Ljava/lang/String;)V",
        &[JValue::Object(&a)],
    )?;
    if with_pkg {
        let s = env.new_string(format!("package:{}", pkg))?;
        let uri = env
            .call_static_method(
                "android/net/Uri",
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&s)],
            )?
            .l()?;
        env.call_method(
            &intent,
            "setData",
            "(Landroid/net/Uri;)Landroid/content/Intent;",
            &[JValue::Object(&uri)],
        )?;
    }
    // FLAG_ACTIVITY_NEW_TASK
    env.call_method(
        &intent,
        "addFlags",
        "(I)Landroid/content/Intent;",
        &[JValue::Int(0x1000_0000)],
    )?;
    env.call_method(
        act,
        "startActivity",
        "(Landroid/content/Intent;)V",
        &[JValue::Object(&intent)],
    )?;
    Ok(())
}

/// يفتح شاشة إعدادات "All files access" الخاصة بالتطبيق
pub fn request_all_files_access() {
    with_env(|env, act| {
        let pkg_obj = env
            .call_method(act, "getPackageName", "()Ljava/lang/String;", &[])?
            .l()?;
        let pkg_jstr = JString::from(pkg_obj);
        let pkg: String = env.get_string(&pkg_jstr)?.into();
        let first = open_settings(
            env,
            act,
            &pkg,
            "android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION",
            true,
        );
        if first.is_err() {
            let _ = env.exception_clear();
            open_settings(
                env,
                act,
                &pkg,
                "android.settings.MANAGE_ALL_FILES_ACCESS_PERMISSION",
                false,
            )?;
        }
        Ok(())
    });
}

/// يخلي نظام الميديا يحدّث الصور (بعد الحذف او الاسترجاع) عشان المعرض يتزامن
fn scan_paths(paths: &[String]) {
    with_env(|env, act| {
        let cls = env.find_class("java/lang/String")?;
        let arr = env.new_object_array(paths.len() as i32, &cls, JObject::null())?;
        for (i, p) in paths.iter().enumerate() {
            let s = env.new_string(p)?;
            env.set_object_array_element(&arr, i as i32, &s)?;
        }
        let null1 = JObject::null();
        let null2 = JObject::null();
        env.call_static_method(
            "android/media/MediaScannerConnection",
            "scanFile",
            "(Landroid/content/Context;[Ljava/lang/String;[Ljava/lang/String;Landroid/media/MediaScannerConnection$OnScanCompletedListener;)V",
            &[
                JValue::Object(act),
                JValue::Object(&arr),
                JValue::Object(&null1),
                JValue::Object(&null2),
            ],
        )?;
        Ok(())
    });
}

pub fn scan_async(paths: Vec<String>) {
    std::thread::spawn(move || scan_paths(&paths));
}
