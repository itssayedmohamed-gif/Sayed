mod android;
mod app;
mod loader;
mod scan;

use android_activity::AndroidApp;

#[no_mangle]
fn android_main(app: AndroidApp) {
    android::init(&app);
    let data_dir = app.internal_data_path();
    let options = eframe::NativeOptions {
        android_app: Some(app),
        ..Default::default()
    };
    let _ = eframe::run_native(
        "PhotoSwipe",
        options,
        Box::new(move |cc| Ok(Box::new(app::PhotoApp::new(cc, data_dir)))),
    );
}
