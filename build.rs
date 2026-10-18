use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-changed=shaders/test_pattern.hlsl");
    println!("cargo:rerun-if-env-changed=PIXSTRIDE_FXC");

    let shader = Path::new("shaders/test_pattern.hlsl");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo 未提供 OUT_DIR"));
    let fxc = find_fxc().unwrap_or_else(|| {
        panic!("找不到 Windows SDK 的 fxc.exe；请安装 Windows 10/11 SDK，或设置 PIXSTRIDE_FXC")
    });

    compile_shader(
        &fxc,
        shader,
        &out_dir.join("test_vs.cso"),
        "VSMain",
        "vs_4_0",
    );
    compile_shader(
        &fxc,
        shader,
        &out_dir.join("test_ps.cso"),
        "PSMain",
        "ps_4_0",
    );
}

fn find_fxc() -> Option<PathBuf> {
    if let Some(path) = env::var_os("PIXSTRIDE_FXC") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    for variable in ["WindowsSdkBinPath", "WindowsSdkDir"] {
        if let Some(root) = env::var_os(variable) {
            let root = PathBuf::from(root);
            for path in [root.join("x64/fxc.exe"), root.join("bin/x64/fxc.exe")] {
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }

    let root = PathBuf::from(env::var_os("ProgramFiles(x86)")?)
        .join("Windows Kits")
        .join("10")
        .join("bin");
    let mut versions: Vec<_> = fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    versions.sort();
    versions.reverse();

    versions
        .into_iter()
        .map(|version| version.join("x64/fxc.exe"))
        .find(|path| path.is_file())
}

fn compile_shader(fxc: &Path, source: &Path, output: &Path, entry: &str, profile: &str) {
    let result = Command::new(fxc)
        .arg("/nologo")
        .arg("/T")
        .arg(profile)
        .arg("/E")
        .arg(entry)
        .arg("/O3")
        .arg("/Ges")
        .arg("/Fo")
        .arg(output)
        .arg(source)
        .output()
        .unwrap_or_else(|error| panic!("启动 fxc.exe 失败：{error}"));

    if !result.status.success() {
        let stdout = String::from_utf8_lossy(&result.stdout);
        let stderr = String::from_utf8_lossy(&result.stderr);
        panic!("fxc.exe 编译 {entry} ({profile}) 失败：\n{stdout}\n{stderr}");
    }
}
