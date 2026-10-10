use hya_stream::hls::{automatic_ffmpeg, ffmpeg, set_ffmpeg_path, valid_ffmpeg_path};

#[test]
fn selected_executable_overrides_discovery_and_reset_restores_it() {
    let automatic = ffmpeg();
    let dir = std::env::temp_dir().join(format!("hydra-selected-ffmpeg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("custom-ffmpeg.exe");
    std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert!(valid_ffmpeg_path(&path));
    assert!(!valid_ffmpeg_path(&dir));
    set_ffmpeg_path(Some(path.clone()));
    assert_eq!(ffmpeg(), Some(path.clone()));
    assert_eq!(automatic_ffmpeg(), automatic);
    assert_eq!(ffmpeg(), Some(path.clone()));
    let missing = dir.join("missing-ffmpeg.exe");
    assert!(!valid_ffmpeg_path(&missing));
    set_ffmpeg_path(Some(missing));
    assert_eq!(ffmpeg(), None);
    set_ffmpeg_path(None);
    assert_eq!(ffmpeg(), automatic);
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(&dir).unwrap();
}
