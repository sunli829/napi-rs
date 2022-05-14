use crate::utils::*;
use cargo_metadata::{MetadataCommand, Package, Target as LibTarget};
use clap::Args;
use clap_cargo::Features;
use log::{error, trace};
use minijinja::{context, Environment};
use rand::{thread_rng, RngCore};
use std::env::{current_dir, temp_dir, var};
use std::fmt::Write;
use std::fs;
use std::path::PathBuf;
use std::process::{exit, Command};

#[derive(Args, Debug, Default)]
/// build the napi-rs crates
pub struct BuildCommandArgs {
  /// Build for the target triple, bypassed to `cargo build --target`
  #[clap(short, long)]
  target: Option<String>,

  /// Path to the `Cargo.toml` manifest
  #[clap(long, parse(from_os_str))]
  cwd: Option<PathBuf>,

  // Directory for all crate generated artifacts, see `cargo build --target-dir`
  #[clap(long, parse(from_os_str))]
  target_dir: Option<PathBuf>,

  /// Path to where all the built files would be put
  #[clap(short, long, parse(from_os_str))]
  output_dir: Option<PathBuf>,

  /// Add platform triple to the generated nodejs binding file, eg: `[name].linux-x64-gnu.node`
  #[clap(long)]
  platform: bool,

  /// Path to the generate JS binding file. Only works with `--target` specified
  #[clap(long = "js")]
  js_binding: Option<String>,

  /// Package name in generated js binding file. Only works with `--target` specified
  #[clap(long)]
  js_package_name: Option<String>,

  /// Disable JS binding file generation
  #[clap(long = "no-js")]
  disable_js_binding: bool,

  /// Path and filename of generated type def file. relative to `--cwd` or `--output_dir` if provided
  #[clap(long, parse(from_os_str))]
  dts: Option<PathBuf>,

  /// Do not output header notes like `// eslint-ignore` to `.d.ts` file
  #[clap(long)]
  no_dts_header: bool,

  /// Whether strip the library to achieve the minimum file size
  #[clap(short, long)]
  strip: bool,

  /// Build in release mode
  #[clap(short, long)]
  release: bool,

  /// Verbosely log build command trace
  #[clap(short, long)]
  verbose: bool,

  /// Build the target as binary
  #[clap(long)]
  bin: bool,

  /// Build the specified library or the one at cwd
  #[clap(short, long)]
  package: Option<String>,

  #[clap(flatten)]
  features: Features,

  /// [experimental] Use `zig` as linker (cross-compile)
  #[clap(short, long)]
  zig: bool,

  /// [experimental] The suffix of zig ABI version. E.g. `--zig-abi-suffix=2.17`
  #[clap(long)]
  zip_abi_suffix: Option<String>,

  /// All other flags bypassed to `cargo build` command. Usage: `napi build -- -p sub-crate`
  #[clap(last = true)]
  bypass_flags: Vec<String>,
}

impl TryFrom<BuildCommandArgs> for BuildCommand {
  type Error = ();

  fn try_from(args: BuildCommandArgs) -> Result<Self, Self::Error> {
    let mut path = args.cwd.clone().unwrap_or_else(|| current_dir().unwrap());
    path.push("Cargo.toml");

    if !path.exists() {
      error!("Could not find Cargo.toml at {:?}", path);
      return Err(());
    }

    match MetadataCommand::new().manifest_path(path).exec() {
      Ok(metadata) => {
        let pkg = metadata.root_package().or_else(|| {
          if let Some(package_arg) = &args.package {
            metadata
              .packages
              .iter()
              .find(|pkg| &pkg.name == package_arg)
          } else {
            None
          }
        });

        match pkg {
          Some(pkg) => Ok(BuildCommand {
            output_dir: args
              .output_dir
              .clone()
              .or_else(|| args.cwd.clone())
              .or_else(|| {
                pkg
                  .manifest_path
                  .parent()
                  .map(|p| p.as_std_path().to_path_buf())
              })
              .unwrap_or_else(|| PathBuf::from("./")),
            target_dir: args
              .target_dir
              .clone()
              .unwrap_or_else(|| metadata.target_directory.clone().into_std_path_buf()),
            lib_target: pkg
              .targets
              .iter()
              .find(|t| t.crate_types.iter().any(|t| t == "cdylib"))
              .cloned(),
            target: args
              .target
              .clone()
              .unwrap_or_else(get_system_default_target),
            intermediate_type_file: get_intermediate_type_file(),
            args,
            package: pkg.clone(),
          }),
          None => {
            error!("Could not find crate to build");
            Err(())
          }
        }
      }
      Err(e) => {
        error!("Could not parse cargo manifest\n{}", e);
        Err(())
      }
    }
  }
}

pub struct BuildCommand {
  args: BuildCommandArgs,
  output_dir: PathBuf,
  target_dir: PathBuf,
  package: Package,
  lib_target: Option<LibTarget>,
  target: String,
  intermediate_type_file: PathBuf,
}

impl Executable for BuildCommand {
  fn execute(&mut self) -> CommandResult {
    if self.args.verbose {
      log::set_max_level(log::LevelFilter::Trace)
    }

    self.run()?;

    Ok(())
  }
}

impl BuildCommand {
  fn run(&self) -> CommandResult {
    self.check_package()?;

    let mut cmd = self.create_command();
    trace!(
      "Running cargo build with args: {:?}",
      cmd
        .get_args()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
    );
    let exit_status = cmd
      .spawn()
      .expect("failed to execute `cargo build`")
      .wait()
      .expect("failed to execute `cargo build`");

    if exit_status.success() {
      self.post_build();
    } else {
      error!("`cargo build` failed");
      exit(exit_status.code().unwrap());
    }

    Ok(())
  }

  fn create_command(&self) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.arg("build");

    self
      .set_cwd(&mut cmd)
      .set_features(&mut cmd)
      .set_target(&mut cmd)
      .set_envs(&mut cmd)
      .set_rust_flags(&mut cmd)
      .set_bypass_args(&mut cmd)
      .set_package(&mut cmd);

    cmd
  }

  fn set_cwd(&self, cmd: &mut Command) -> &Self {
    if let Some(cwd) = &self.args.cwd {
      trace!("set cargo working dir to {}", cwd.display());
      cmd.current_dir(cwd);
    }

    self
  }

  fn set_envs(&self, cmd: &mut Command) -> &Self {
    let envs = vec![(
      "TYPE_DEF_TMP_PATH",
      self.intermediate_type_file.to_str().unwrap(),
    )];

    trace!("set environment variables: ");
    envs.iter().for_each(|(k, v)| {
      trace!("{}={}", k, v);
      cmd.env(k, v);
    });

    self
  }

  fn set_target(&self, cmd: &mut Command) -> &Self {
    trace!("set compiling target to {}", &self.target);
    cmd.arg("--target").arg(&self.target);

    self
  }

  fn set_bypass_args(&self, cmd: &mut Command) -> &Self {
    trace!("bypassing flags: {:?}", self.args.bypass_flags);

    if self.args.release {
      cmd.arg("--release");
    }

    if self.args.target_dir.is_some() {
      cmd
        .arg("--target-dir")
        .arg(self.args.target_dir.as_ref().unwrap());
    }

    cmd.args(self.args.bypass_flags.iter());

    self
  }

  fn set_features(&self, cmd: &mut Command) -> &Self {
    let mut args = vec![];
    if self.args.features.all_features {
      args.push(String::from("--all-features"))
    } else if self.args.features.no_default_features {
      args.push(String::from("--no-default-features"))
    } else if !self.args.features.features.is_empty() {
      args.push(String::from("--features"));
      args.extend_from_slice(&self.args.features.features);
    }

    trace!("set features flags: {:?}", args);
    cmd.args(args);

    self
  }

  fn set_package(&self, cmd: &mut Command) -> &Self {
    let mut args = vec![];

    if let Some(package) = &self.args.package {
      args.push("-p");
      args.push(package.as_ref());
    }

    if self.args.bin {
      args.push("--bin");
    }

    if !args.is_empty() {
      trace!("set package flags: {:?}", args);
      cmd.args(args);
    }

    self
  }

  fn set_rust_flags(&self, cmd: &mut Command) -> &Self {
    let mut rust_flags = match var("RUSTFLAGS") {
      Ok(s) => s,
      Err(_) => String::new(),
    };

    if self.target.contains("musl") && !rust_flags.contains("target-feature=-crt-static") {
      rust_flags.push_str(" -C target-feature=-crt-static");
    }

    if self.args.strip && !rust_flags.contains("link-arg=-s") {
      rust_flags.push_str(" -C link-arg=-s");
    }

    if !rust_flags.is_empty() {
      trace!("set RUSTFLAGS: {}", rust_flags);
      cmd.env("RUSTFLAGS", rust_flags);
    }

    self
  }

  fn check_package(&self) -> CommandResult {
    if self.args.bin {
      return Ok(());
    }

    if self.lib_target.is_none() {
      error!("crate is not a cdylib");
      return Err(());
    }

    Ok(())
  }

  fn post_build(&self) {
    self.copy_output();
    self.process_type_def();
    self.write_js_binding();
  }

  fn copy_output(&self) {
    let mut src = self.target_dir.clone();
    let mut dest = self.output_dir.clone();

    src.push(&self.target);
    src.push(if self.args.release {
      "release"
    } else {
      "debug"
    });

    let (src_name, dest_name) = self.get_artifact_names();
    src.push(src_name);
    dest.push(dest_name);

    if let Ok(()) = fs::remove_file(&dest) {};
    if let Err(e) = fs::copy(&src, &dest) {
      error!("Failed to move artifact to dest path. {}", e);
    };
  }

  fn get_artifact_names(&self) -> (/* src */ String, /* dist */ String) {
    let target = Target::from(&self.target);
    let is_binary = self.args.bin;
    let name = if is_binary {
      self.package.name.clone()
    } else {
      self
        .lib_target
        .as_ref()
        .unwrap()
        .name
        .clone()
        .replace('-', "_")
    };

    let src_name = if is_binary {
      if target.platform == NodePlatform::Windows {
        format!("{}.exe", name)
      } else {
        name
      }
    } else {
      match target.platform {
        NodePlatform::Darwin => {
          format!("lib{}.dylib", name)
        }
        NodePlatform::Windows => {
          format!("{}.dll", name)
        }
        _ => {
          format!("lib{}.so", name)
        }
      }
    };

    let dest_name = if is_binary {
      src_name.clone()
    } else {
      format!(
        "{}{}.node",
        "index",
        if self.args.platform {
          format!(".{}", target.platform_arch_abi)
        } else {
          "".to_owned()
        }
      )
    };

    (src_name, dest_name)
  }

  fn process_type_def(&self) {
    if !self.intermediate_type_file.exists() {
      return;
    }

    let mut dest = self.output_dir.clone();
    match &self.args.dts {
      Some(dts) => dest.push(dts),
      None => dest.push("index.d.ts"),
    };

    let type_def_file = IntermidiateTypeDefFile::from(&self.intermediate_type_file);
    let dts = type_def_file
      .into_dts(!self.args.no_dts_header)
      .expect("Failed to parse type def file");

    write_file(&dest, &dts).expect("Failed to write type def file");
  }

  fn write_js_binding(&self) {
    if !self.args.platform || self.args.disable_js_binding {
      return;
    }

    let mut output = self.output_dir.clone();
    output.push(
      self
        .args
        .js_binding
        .clone()
        .unwrap_or_else(|| String::from("index.js")),
    );

    let mut env = Environment::new();
    env
      .add_template("index.js", include_str!("./templates/binding.tpl"))
      .unwrap();

    let binding = env
      .get_template("index.js")
      .and_then(|template| {
        template.render(context!(
          binary_name => self.package.name.clone(),
          package_name => self.args.js_package_name.clone().unwrap_or_else(|| self.package.name.clone())
        ))
      })
      .expect("Failed to generate js binding file.");

    write_file(&output, &binding).expect("Failed to write js binding file");
  }
}

fn get_intermediate_type_file() -> PathBuf {
  let len = 16;
  let mut rng = thread_rng();
  let mut data = vec![0; len];
  rng.fill_bytes(&mut data);

  let mut hex_string = String::with_capacity(2 * len);
  for byte in data {
    write!(hex_string, "{:02X}", byte).unwrap();
  }

  temp_dir().join(format!("type_def.{hex_string}.tmp"))
}
