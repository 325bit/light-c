fn main() {
    // tauri-build embeds the app manifest only into the bin target. Disable that and inject
    // the manifest ourselves: otherwise the bin target receives two MANIFEST resources and
    // linking fails with CVT1100: duplicate resource.
    let attributes = tauri_build::Attributes::new()
        .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    tauri_build::try_build(attributes).expect("tauri-build 执行失败");

    link_app_manifest_into_every_target();
}

/// Embed the Windows application manifest into **every** linked target, including the lib test binary.
///
/// Background: from tauri 2.12, `common-controls-v6` is a default feature, so the lib statically
/// references `comctl32!TaskDialogIndirect` (used by muda's About dialog). That export exists only
/// in ComCtl32 v6, which is activated only when the application manifest declares a dependency on
/// `Microsoft.Windows.Common-Controls`. Without the manifest, the v5 copy in System32 is loaded and
/// the process exits at startup with STATUS_ENTRYPOINT_NOT_FOUND (0xC0000139) -- no test can even run.
///
/// `/MANIFESTINPUT` **merges** our manifest into the linker-generated default one, preserving
/// UAC, DPI, and other sections, so the final bin manifest is equivalent to what tauri-build embedded.
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

/// Matches tauri-build's built-in manifest: declares only the ComCtl32 v6 dependency.
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
