use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

const HELPER_NAME: &str = "Oxide Notifications.app";
const HELPER_IDENTIFIER: &str = "com.oxide.desktop.notifications";
const PLIST_BUDDY: &str = "/usr/libexec/PlistBuddy";

static HELPER: OnceLock<Result<PathBuf, String>> = OnceLock::new();

pub(crate) fn show(title: &str, body: &str) -> Result<(), String> {
    let helper = HELPER.get_or_init(resolve_helper).clone()?;
    let output = Command::new("/usr/bin/open")
        .arg("-g")
        .arg("-n")
        .arg(&helper)
        .arg("--args")
        .args([title, body])
        .output()
        .map_err(|error| format!("could not launch {}: {error}", helper.display()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_error("launch notification helper", &output))
    }
}

fn resolve_helper() -> Result<PathBuf, String> {
    let contents = app_contents_dir()?;
    let source_icon = contents.join("Resources/oxide.icns");
    if !source_icon.is_file() {
        return Err(format!(
            "Oxide icon is missing under {}",
            contents.display()
        ));
    }

    let bundled = contents.join("Helpers").join(HELPER_NAME);
    if helper_is_current(&bundled, &source_icon) {
        return Ok(bundled);
    }

    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is unavailable for notification helper migration".to_string())?;
    let helper = home
        .join("Library/Application Support/Oxide/Helpers")
        .join(HELPER_NAME);
    if helper_is_current(&helper, &source_icon) {
        return Ok(helper);
    }

    build_notification_helper(&helper, &source_icon)?;
    Ok(helper)
}

fn app_contents_dir() -> Result<PathBuf, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("could not resolve the Oxide executable: {error}"))?;
    let macos = executable
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", executable.display()))?;
    let contents = macos
        .parent()
        .ok_or_else(|| format!("{} is not inside an app bundle", executable.display()))?;
    if macos.file_name().and_then(|name| name.to_str()) != Some("MacOS")
        || contents.file_name().and_then(|name| name.to_str()) != Some("Contents")
    {
        return Err(format!(
            "{} is not inside a macOS application bundle",
            executable.display()
        ));
    }
    Ok(contents.to_path_buf())
}

fn build_notification_helper(helper: &Path, source_icon: &Path) -> Result<(), String> {
    let parent = helper
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", helper.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create {}: {error}", parent.display()))?;

    let staging = parent.join(format!(
        ".oxide-notification-helper-{}.app",
        std::process::id()
    ));
    remove_managed_directory(&staging, "notification helper staging")?;

    let result = build_notification_helper_staging(&staging, source_icon).and_then(|()| {
        remove_managed_directory(helper, "stale notification helper")?;
        fs::rename(&staging, helper).map_err(|error| {
            format!(
                "could not activate notification helper {}: {error}",
                helper.display()
            )
        })?;
        Ok(())
    });

    if result.is_err() {
        let _ = remove_managed_directory(&staging, "failed notification helper staging");
    }
    result
}

fn remove_managed_directory(path: &Path, label: &str) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "could not inspect {label} {}: {error}",
                path.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "refusing to replace {label} because {} is not a regular directory",
            path.display()
        ));
    }
    fs::remove_dir_all(path)
        .map_err(|error| format!("could not remove {label} {}: {error}", path.display()))
}

fn build_notification_helper_staging(staging: &Path, source_icon: &Path) -> Result<(), String> {
    let output = Command::new("/usr/bin/osacompile")
        .arg("-o")
        .arg(staging)
        .args([
            "-e",
            "on run argv",
            "-e",
            "if (count of argv) < 2 then return",
            "-e",
            "display notification (item 2 of argv) with title (item 1 of argv)",
            "-e",
            "end run",
        ])
        .output()
        .map_err(|error| format!("could not compile notification helper: {error}"))?;
    require_success("compile notification helper", &output)?;

    let resources = staging.join("Contents/Resources");
    let helper_icon = resources.join("oxide.icns");
    fs::copy(source_icon, &helper_icon).map_err(|error| {
        format!(
            "could not copy Oxide icon from {} to {}: {error}",
            source_icon.display(),
            helper_icon.display()
        )
    })?;

    let plist = staging.join("Contents/Info.plist");
    let _ = plist_command(&plist, "Delete :CFBundleIconName");
    set_plist_value(&plist, "CFBundleIdentifier", "string", HELPER_IDENTIFIER)?;
    set_plist_value(&plist, "CFBundleName", "string", "Oxide")?;
    set_plist_value(&plist, "CFBundleDisplayName", "string", "Oxide")?;
    set_plist_value(&plist, "CFBundleIconFile", "string", "oxide.icns")?;
    set_plist_value(
        &plist,
        "CFBundleVersion",
        "string",
        env!("CARGO_PKG_VERSION"),
    )?;
    set_plist_value(
        &plist,
        "CFBundleShortVersionString",
        "string",
        env!("CARGO_PKG_VERSION"),
    )?;
    set_plist_value(&plist, "LSUIElement", "bool", "true")?;

    let output = Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-", "--identifier", HELPER_IDENTIFIER])
        .arg(staging)
        .output()
        .map_err(|error| format!("could not sign notification helper: {error}"))?;
    require_success("sign notification helper", &output)?;

    if !helper_is_current(staging, source_icon) {
        return Err("notification helper verification failed after migration".to_string());
    }
    Ok(())
}

fn helper_is_current(helper: &Path, source_icon: &Path) -> bool {
    if !helper.is_dir() {
        return false;
    }
    let plist = helper.join("Contents/Info.plist");
    let helper_icon = helper.join("Contents/Resources/oxide.icns");
    files_equal(source_icon, &helper_icon)
        && plist_value(&plist, "CFBundleIdentifier").as_deref() == Some(HELPER_IDENTIFIER)
        && plist_value(&plist, "CFBundleIconFile").as_deref() == Some("oxide.icns")
        && plist_value(&plist, "CFBundleVersion").as_deref() == Some(env!("CARGO_PKG_VERSION"))
        && codesign_identifier(helper).as_deref() == Some(HELPER_IDENTIFIER)
}

fn files_equal(left: &Path, right: &Path) -> bool {
    match (fs::read(left), fs::read(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn set_plist_value(plist: &Path, key: &str, kind: &str, value: &str) -> Result<(), String> {
    let set = format!("Set :{key} {value}");
    if plist_command(plist, &set).is_ok_and(|output| output.status.success()) {
        return Ok(());
    }

    let add = format!("Add :{key} {kind} {value}");
    let output = plist_command(plist, &add)?;
    require_success(&format!("set {key} in notification helper plist"), &output)
}

fn plist_value(plist: &Path, key: &str) -> Option<String> {
    let output = plist_command(plist, &format!("Print :{key}")).ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn plist_command(plist: &Path, command: &str) -> Result<Output, String> {
    Command::new(PLIST_BUDDY)
        .arg("-c")
        .arg(command)
        .arg(plist)
        .output()
        .map_err(|error| format!("could not update {}: {error}", plist.display()))
}

fn codesign_identifier(helper: &Path) -> Option<String> {
    let output = Command::new("/usr/bin/codesign")
        .args(["-d", "--verbose=4"])
        .arg(helper)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .find_map(|line| line.strip_prefix("Identifier=").map(str::to_string))
}

fn require_success(operation: &str, output: &Output) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        Err(command_error(operation, output))
    }
}

fn command_error(operation: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!(
        "{operation} failed with status {}: {}",
        output.status,
        stderr.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn migrated_helper_keeps_oxide_icon_and_identity() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "oxide-notification-helper-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let source_icon = root.join("oxide.icns");
        fs::write(&source_icon, b"oxide-icon-fixture").unwrap();
        let helper = root.join(HELPER_NAME);

        build_notification_helper(&helper, &source_icon).unwrap();

        assert!(helper_is_current(&helper, &source_icon));
        assert_eq!(
            fs::read(helper.join("Contents/Resources/oxide.icns")).unwrap(),
            b"oxide-icon-fixture"
        );
        assert_eq!(
            plist_value(&helper.join("Contents/Info.plist"), "CFBundleIconName"),
            None
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn managed_cleanup_refuses_directory_symlinks() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "oxide-notification-helper-symlink-test-{}-{nonce}",
            std::process::id()
        ));
        let target = root.join("target");
        let link = root.join("helper.app");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("keep"), b"keep").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let error = remove_managed_directory(&link, "test helper").unwrap_err();

        assert!(error.contains("refusing to replace"));
        assert!(target.join("keep").is_file());
        fs::remove_file(link).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
