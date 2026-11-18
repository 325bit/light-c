fn main() {
    // tauri-build 默认只把应用清单嵌进 bin 目标。这里换成“自己注入”：先让它不嵌，
    // 再统一给所有目标补清单，否则 bin 目标会同时拿到两份 MANIFEST 资源，
    // 链接时报 CVT1100: duplicate resource。
    let attributes = tauri_build::Attributes::new()
        .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    tauri_build::try_build(attributes).expect("tauri-build 执行失败");

    link_app_manifest_into_every_target();
}

/// 让**所有**链接目标（含 lib 单测二进制）都带上 Windows 应用清单。
///
/// 背景：tauri 2.12 起 `common-controls-v6` 进入默认特性，lib 因此静态引用了
/// `comctl32!TaskDialogIndirect`（muda 的“关于”对话框用它）。该导出只存在于
/// ComCtl32 v6，而 v6 必须在应用清单里声明 `Microsoft.Windows.Common-Controls`
/// 依赖才会被激活；缺清单时加载的是 System32 下的 v5，进程在启动阶段就以
/// STATUS_ENTRYPOINT_NOT_FOUND (0xC0000139) 退出，一个用例都跑不到。
///
/// `/MANIFESTINPUT` 会把清单**合并**进链接器生成的默认清单，UAC、DPI 等节都保留，
/// 所以 bin 目标的最终清单与 tauri-build 原先嵌入的那份等价。
fn link_app_manifest_into_every_target() {
    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    {
        use std::path::PathBuf;

        let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR 未设置"));
        let manifest_path = out_dir.join("lightc-app-manifest.xml");
        std::fs::write(&manifest_path, APP_MANIFEST).expect("写入应用清单失败");

        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg=/MANIFESTINPUT:{}",
            manifest_path.display()
        );
    }
}

/// 与 tauri-build 内置清单一致：只声明 ComCtl32 v6 依赖。
#[cfg(all(target_os = "windows", target_env = "msvc"))]
const APP_MANIFEST: &str = r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity
        type="win32"
        name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0"
        processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df"
        language="*"
      />
    </dependentAssembly>
  </dependency>
</assembly>
"#;
