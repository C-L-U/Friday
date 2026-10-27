//! Opt-in native keychain check with a temporary synthetic credential.
//! Never reads or writes Jarvis's production credential entries.
fn main() -> anyhow::Result<()> {
    if let Some(user) = std::env::args().nth(1) {
        let entry = keyring::Entry::new("com.jarvis.voice.test", &user)?;
        anyhow::ensure!(
            entry.get_password()? == "synthetic-keychain-probe",
            "Child process could not read credential"
        );
        return Ok(());
    }
    let unique = format!(
        "probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    let entry = keyring::Entry::new("com.jarvis.voice.test", &unique)?;
    entry.set_password("synthetic-keychain-probe")?;
    let read = entry.get_password();
    let restarted = std::process::Command::new(std::env::current_exe()?)
        .arg(&unique)
        .status();
    let removed = entry.delete_credential();
    anyhow::ensure!(
        read?.as_str() == "synthetic-keychain-probe",
        "Keychain did not return the test value"
    );
    removed?;
    anyhow::ensure!(
        restarted?.success(),
        "Credential did not survive process restart"
    );
    println!("Native keychain write/read/new-process-read/delete passed; no production credentials accessed.");
    Ok(())
}
