// ============================================================================
// WhatsApp Windows data path adapter
// ============================================================================
//
// WhatsApp Desktop is Electron-based; common data lives in LocalAppData's packages,
// the Roaming WhatsApp directory, or the user profile. Only Cache/Media/Attachments
// are candidates for cleanup; IndexedDB/Databases keep the chat-database protection level.
// ============================================================================

use super::core::{FileCategory, SocialAppPath, SocialScanner};
use std::path::{Path, PathBuf};

impl SocialScanner {
    /// Finds WhatsApp Desktop's cache, media, and database directories.
    pub(super) fn detect_whatsapp_paths(&self) -> Option<Vec<SocialAppPath>> {
        let mut paths = Vec::new();
        let bases = [
            PathBuf::from(&self.appdata).join("WhatsApp"),
            PathBuf::from(&self.local_appdata).join("WhatsApp"),
            PathBuf::from(&self.appdata).join(r"WhatsApp\Cache"),
        ];

        for base in bases {
            self.add_whatsapp_paths(&base, &mut paths);
        }

        for package in self.find_package_directories(&["whatsapp", "5319275a"]) {
            self.add_whatsapp_paths(&package.join("LocalCache"), &mut paths);
            self.add_whatsapp_paths(&package.join("LocalState"), &mut paths);
            self.add_whatsapp_paths(&package.join(r"LocalCache\Roaming\WhatsApp"), &mut paths);
        }

        (!paths.is_empty()).then_some(paths)
    }

    fn add_whatsapp_paths(&self, base: &Path, paths: &mut Vec<SocialAppPath>) {
        self.add_named_subdirectories(
            base,
            &["Cache", "cache", "Code Cache", "GPUCache", "Service Worker"],
            "WhatsApp",
            FileCategory::TempCache,
            false,
            paths,
        );
        self.add_named_subdirectories(
            base,
            &["Media", "Images", "Videos", "Stickers"],
            "WhatsApp",
            FileCategory::ImageVideo,
            false,
            paths,
        );
        self.add_named_subdirectories(
            base,
            &["Attachments", "Documents"],
            "WhatsApp",
            FileCategory::FileTransfer,
            false,
            paths,
        );
        self.add_named_subdirectories(
            base,
            &["Databases", "IndexedDB", "Local Storage"],
            "WhatsApp",
            FileCategory::ChatDatabase,
            false,
            paths,
        );
    }
}
