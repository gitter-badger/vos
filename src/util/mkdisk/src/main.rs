#![feature(slice_bytes, path_relative_from)]

use std::fs::File;
use std::path::PathBuf;
use std::ops::DerefMut;

extern crate byteorder;
extern crate docopt;
extern crate env_logger;
#[macro_use] extern crate log;

use docopt::Docopt;

mod disk;
mod fs;

use fs::*;
use disk::*;

static VERSION: &'static str = "0.0.1";
static USAGE: &'static str = "
Usage: mkdisk [options] <dir>

Creates a bootable disk image.

Options:
    -h, --help     Print this help message
    -v, --version  Print the version of mkdisk
    -s, --size=SIZE         The fixed size of the disk image [default: 4MiB]
    -o, --out=FILE          The output disk image file
    -b, --bootloader=FILE   The bootloader to use for the first few sectors

File sizes measured using KB = 1000, KiB=1024 etc
";

fn main() {
    env_logger::init().unwrap();

    let args: docopt::ArgvMap = Docopt::new(USAGE)
                      .and_then(|d| d.help(true)
                                     .version(Some(VERSION.into()))
                                     .parse())
                      .unwrap_or_else(|e| e.exit());


    let mut config = Config::new(args);
    config.exec();
}

struct Config {
    dsize: usize,
    src: PathBuf,

    boot_path: PathBuf,
    boot: File,

    out_path: PathBuf,
    out: File,
}

impl Config {
    pub fn new(args: docopt::ArgvMap) -> Config {
        // default is 4MiB, as specified in USAGE
        let dsize = parse_size(args.get_str("-s"));

        let boot_path: PathBuf = match args.get_str("-b") {
            ""   => panic!("Bootloader unspecified: use `-b` or `--bootloader`"),
            path => {
                path.into()
            },
        };
        let boot = File::open(&boot_path)
                        .unwrap_or_else(|e| panic!("Unable to open bootloader `{}`: {}",
                                                   args.get_str("-b"), e));

        let src = args.get_str("<dir>").into();
        let out_path = PathBuf::from(match args.get_str("-o") {
            "" => {
                // if source dir is `bin/fs/`, then the output file becomes `bin/fs.disk`
                let mut path = PathBuf::from(&src);
                path.set_extension("disk");
                path
            },
            s  => PathBuf::from(s),
        });

        let out = File::create(&out_path)
                       .unwrap_or_else(|e| panic!("Unable to open output file `{:?}`: {}", out_path, e));

        Config {
            dsize: dsize,
            src: src,

            boot_path: boot_path,
            boot: boot,

            out_path: out_path,
            out: out,
        }
    }

    pub fn exec(&mut self) {
        use std::io::Read;
        use std::ops::Deref;

        let smeta = ::std::fs::metadata(&self.src)
                              .unwrap_or_else(|e| panic!("Unable to query source `{}`: {}", &self.src.display(), e));
        if !smeta.is_dir() {
            panic!("Source path is not a folder: `{}`", &self.src.display());
        }

        let sectors = self.dsize / 512;

        // ensure room for filesystem
        assert!(sectors >= 128, "Minimum disk size is 64KiB");

        let mut disk = RamDisk::new(sectors);

        let mut i = 0;
        loop {
            let mut sector: [u8; 512] = [0; 512];
            match self.boot.read(&mut sector) {
                // Ok(n) if n == 512 => { }, // read sector fine
                // Ok(n) if n < 512  => { break; } // finished reading bootloader
                Ok(0) => { break; }
                Ok(n) => { }
                Err(e) => { panic!("Unable to read bootloader `{}`: {}", self.out_path.display(), e); },
                // _ => unreachable!(),
            }
            disk.write_sector(i, &sector);
            i += 1;
        }


        let pinfo = PartitionInfo {
            format: Format::Fat32,
            size: sectors - 64,
            start: 64, // historically, partitions are aligned to cylinder boundaries, so start on sector 64
            bootable: true,
        };
        disk::set_pinfo(&mut disk, 0, &pinfo).unwrap();

        {
            let mut partition = disk::get_partition(&mut disk, 0).unwrap();
            fs::fat::format(&mut partition).unwrap();
        }


        fn recurse<T: FileSystem + ?Sized>(fs: &mut T, src: &PathBuf, dir: PathBuf) {
            for item in ::std::fs::read_dir(&dir).unwrap() {
                let item = item.unwrap(); // TODO unwrap
                let rpath = item.path();
                let rc = rpath.clone();
                let vpath = rc.relative_from(src).unwrap();

                debug!("Config::exec::recurse() vpath: {:?}", &vpath);
                let ft = item.file_type().unwrap();
                if ft.is_dir() {
                    fs.make_dir(vpath).unwrap();
                    recurse(fs, src, rpath);
                } else if ft.is_file() {
                    use std::io::Read;

                    let mut file = match File::open(&rpath) {
                        Ok(f) => f,
                        Err(e) => panic!("Cannot open file `{:?}`: {}", rpath, e),
                    };

                    let mut v = Vec::new();
                    file.read_to_end(&mut v).unwrap();

                    fs.write_file(vpath, &v);
                }
            }
        }

        let mut fs = disk::mount(&mut disk, 0).unwrap();
        recurse(fs.deref_mut(), &self.src, self.src.clone());

        for sector in &*disk {
            use std::io::Write;
            self.out.write(sector);
        }
    }
}

// TODO: error handling
fn parse_size(s: &str) -> usize {
    let mut num = s.chars().take_while(|c| c.is_numeric())
                           .collect::<String>()
                           .parse::<usize>()
                           .unwrap_or_else(|_| panic!("Unable to interpret size: `{}`", s));

    const UNITS: [(&'static str, usize); 4] = [
        // update test::parse_size() when expanding
        ("kb",     1_000), ("kib", 1 << 10),
        ("mb", 1_000_000), ("mib", 1 << 20),
    ];

    let mut success = false;
    let unit = s.chars().skip_while(|c| c.is_numeric())
                        .collect::<String>()
                        .to_lowercase();

    // TODO: refactor
    for &(suffix, factor) in &UNITS {
        if unit == suffix {
            success = true;
            num *= factor;
            break;
        }
    }
    if !success {
        panic!("Unknown unit suffix: `{}`", unit);
    }

    num
}

#[cfg(test)]
mod test {
    #[test]
    fn parse_size() {
        assert_eq!(super::parse_size("1mib"),2 << 20);
        assert_eq!(super::parse_size("4MiB"),4 * 2 << 20);
        assert_eq!(super::parse_size("35kb"), 35 * 1_000);
    }
}