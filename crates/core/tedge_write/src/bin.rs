// TODO: force `destination_path` to be the first argument in clap

use anyhow::bail;
use anyhow::Context;
use camino::Utf8PathBuf;
use clap::Parser;
use tedge_config::cli::CommonArgs;
use tedge_config::log_init;
use tedge_utils::atomic::MaybePermissions;
use std::os::unix::fs::PermissionsExt;

/// tee-like helper for writing to files which `tedge` user does not have write permissions to.
///
/// To be used in combination with sudo, passing the file content via standard input.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
#[command(about, version, long_about)]
pub struct Args {
    /// A canonical path to a file to which standard input will be written.
    ///
    /// If the file does not exist, it will be created with the specified owner/group/permissions.
    /// If the file does exist, it will be overwritten, but its owner/group/permissions will remain
    /// unchanged.
    destination_path: Utf8PathBuf,

    /// Permission mode for the file, in octal form.
    #[arg(long)]
    mode: Option<Box<str>>,

    /// User which will become the new owner of the file (and for the paths with --makedirs).
    #[arg(long)]
    user: Option<Box<str>>,

    /// Group which will become the new owner of the file (and for the paths with --makedirs).
    #[arg(long)]
    group: Option<Box<str>>,

    /// Use to create intermediate paths when needed. 
    /// Created paths will have the permission 0755 and owner as specified by --user and --group.
    #[arg(long, default_value_t = false)]
    makedirs: bool,

    #[command(flatten)]
    common: CommonArgs,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    log_init(
        "tedge-write",
        &args.common.log_args,
        &args.common.config_dir,
    )?;

    // /etc/sudoers can contain rules where sudo permissions are given to `tedge` user depending on
    // the files we write to, e.g:
    //
    // `tedge    ALL = (ALL) NOPASSWD: /usr/bin/tedge-write /etc/*`
    //
    // If the destination path contains `..` then we can "escape" outside the directory we're
    // allowed to write to. For that reason, we require paths to be canonical.
    //
    // Ideally this would be solved by a more expressive filesystem permissions system, e.g. ACL,
    // but unfortunately they're not standard on Linux, so we're stuck with trying to do next best
    // thing with sudo.
    if !args.destination_path.is_absolute() {
        bail!("Destination path has to be absolute");
    }

    // unwrap is safe because clean returns an utf8 path when given an utf8 path
    let target_filepath: Utf8PathBuf = path_clean::clean(args.destination_path.as_std_path())
        .try_into()
        .unwrap();

    if target_filepath != *args.destination_path {
        bail!(
            "Destination path {} is not canonical",
            args.destination_path
        );
    }

    let mode = args
        .mode
        .map(|m| u32::from_str_radix(&m, 8).with_context(|| format!("invalid mode: {m}")))
        .transpose()?;

    let uid = args
        .user
        .map(|u| uzers::get_user_by_name(&*u).with_context(|| format!("no such user: '{u}'")))
        .transpose()?
        .map(|u| u.uid());

    let gid = args
        .group
        .map(|g| uzers::get_group_by_name(&*g).with_context(|| format!("no such group: '{g}'")))
        .transpose()?
        .map(|g| g.gid());

    if args.makedirs {
        let dir = target_filepath.parent().unwrap();       
        if !dir.exists() {

            let mut current = Utf8PathBuf::new();
            for comp in dir.components() {
                current.push(comp);

                if current.exists() {
                    continue;
                }

                std::fs::create_dir(&current)
                    .context(format!("failed to create directory '{current:?}'"))?;

                let mode = 0o755;    // owner can do all, group, others can enter/read
                let perm = std::fs::Permissions::from_mode(mode);
                std::fs::set_permissions(&current, perm)
                    .context(format!("failed to set permissions {mode:o} on directory '{current:?}'"))?;

                if uid.is_some() || gid.is_some() {
                    std::os::unix::fs::chown(&current, uid, gid)
                        .context(format!("failed to change ownership {:?}:{:?} on directory '{current:?}'", uid, gid))?;
                }
            }
        }
    }    

    // what permissions we want to set if the file doesn't exist
    let permissions = MaybePermissions { uid, gid, mode };

    let src = std::io::stdin().lock();

    tedge_utils::atomic::write_file_atomic_set_permissions_if_doesnt_exist(
        src,
        &target_filepath,
        &permissions,
    )
    .with_context(|| format!("failed to write to destination file '{target_filepath}'"))?;

    Ok(())
}

