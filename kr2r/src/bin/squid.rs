use clap::Parser;
use kr2r::compact_hash::{CHPage, CHTable, Compact, HashConfig, K2Compact, Slot};
use kr2r::utils::find_and_sort_files;
// use std::collections::HashMap;
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Result, Write};
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

// 定义每批次处理的 Slot 数量
const BATCH_SIZE: usize = 8 * 1024 * 1024;

/// Command line arguments for the splitr program.
///
/// This structure defines the command line arguments that are accepted by the splitr program.
/// It uses the `clap` crate for parsing command line arguments.
#[derive(Parser, Debug, Clone)]
#[clap(version, about = "Squid", long_about = "classify a set of sequences")]
struct Args {
    /// The file path for the Kraken 2 index.
    #[clap(short = 'H', long = "index-filename", value_parser, required = true)]
    index_filename: PathBuf,

    /// The file path for the Kraken 2 options.
    // #[clap(short = 'o', long = "options-filename", value_parser, required = true)]
    // options_filename: String,

    /// chunk directory
    #[clap(long)]
    chunk_dir: PathBuf,

    /// 批量处理大小 default: 8MB
    #[clap(long, default_value_t = BATCH_SIZE)]
    batch_size: usize,
}

fn read_chunk_header<R: Read>(reader: &mut R) -> io::Result<(usize, usize)> {
    let mut buffer = [0u8; 16]; // u64 + u64 = 8 bytes + 8 bytes

    reader.read_exact(&mut buffer)?;

    let index = u64::from_le_bytes(
        buffer[0..8]
            .try_into()
            .expect("Failed to convert bytes to u64 for index"),
    );
    let chunk_size = u64::from_le_bytes(
        buffer[8..16]
            .try_into()
            .expect("Failed to convert bytes to u64 for chunk size"),
    );

    Ok((index as usize, chunk_size as usize))
}

fn write_to_file(
    file_index: u64,
    bytes: &[u8],
    last_file_index: &mut Option<u64>,
    writer: &mut Option<BufWriter<File>>,
    chunk_dir: &PathBuf,
) -> io::Result<()> {
    if last_file_index.is_none() || last_file_index.unwrap() != file_index {
        if let Some(mut w) = writer.take() {
            w.flush()?;
        }

        let file_name = format!("sample_file_{}.bin", file_index);
        let file_path = chunk_dir.join(file_name);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)?;
        *writer = Some(BufWriter::new(file));

        *last_file_index = Some(file_index);
    }

    if let Some(w) = writer.as_mut() {
        w.write_all(bytes)?;
    }

    Ok(())
}

fn process_batch<R, K>(
    reader: &mut R,
    chtm: &K,
    chunk_dir: PathBuf,
    batch_size: usize,
) -> std::io::Result<()>
where
    K: K2Compact<u32> + Send,
    R: Read + Send,
{
    let slot_size = std::mem::size_of::<Slot<u64>>();
    let mut batch_buffer = vec![0u8; slot_size * batch_size];
    let mut last_file_index: Option<u64> = None;
    let mut writer: Option<BufWriter<File>> = None;

    let value_mask = chtm.get_value_mask();
    let value_bits = chtm.get_value_bits();

    while let Ok(bytes_read) = reader.read(&mut batch_buffer) {
        if bytes_read == 0 {
            break;
        } // 文件末尾

        // 处理读取的数据批次
        let slots_in_batch = bytes_read / slot_size;

        let slots = unsafe {
            std::slice::from_raw_parts(batch_buffer.as_ptr() as *const Slot<u64>, slots_in_batch)
        };

        let result: HashMap<u64, Vec<u8>> = slots
            .into_par_iter()
            .filter_map(|slot| {
                let taxid = chtm.get_from_page(slot);

                if taxid > 0 {
                    let file_index = slot.value.right(value_mask) >> 32;
                    let left = slot.value.left(value_bits) as u32;
                    let high = u32::combined(left, taxid, value_bits) as u64;
                    let value = slot.to_b(high);
                    let value_bytes = value.to_le_bytes(); // 将u64转换为[u8; 8]
                    Some((file_index, value_bytes.to_vec()))
                } else {
                    None
                }
            })
            .fold(
                || HashMap::new(),
                |mut acc: HashMap<u64, Vec<u8>>, (file_index, value_bytes)| {
                    acc.entry(file_index)
                        .or_insert_with(Vec::new)
                        .extend(value_bytes);
                    acc
                },
            )
            .reduce(
                || HashMap::new(),
                |mut acc, h| {
                    for (k, mut v) in h {
                        acc.entry(k).or_insert_with(Vec::new).append(&mut v);
                    }
                    acc
                },
            );

        let mut file_indices: Vec<_> = result.keys().cloned().collect();
        file_indices.sort_unstable(); // 对file_index进行排序

        for file_index in file_indices {
            if let Some(bytes) = result.get(&file_index) {
                write_to_file(
                    file_index,
                    bytes,
                    &mut last_file_index,
                    &mut writer,
                    &chunk_dir,
                )?;
            }
        }
    }

    if let Some(w) = writer.as_mut() {
        w.flush()?;
    }

    Ok(())
}

fn process_chunk_file<P: AsRef<Path>>(
    args: &Args,
    chunk_file: P,
    hash_files: &Vec<PathBuf>,
) -> Result<()> {
    let file = File::open(chunk_file)?;
    let mut reader = BufReader::new(file);

    let (page_index, page_size) = read_chunk_header(&mut reader)?;

    let start = Instant::now();

    if hash_files.len() <= 1 {
        let chtm = CHTable::<u32>::from(&hash_files[0], page_index, page_size)?;
        // 计算持续时间
        let duration = start.elapsed();
        // 打印运行时间
        println!("load table took: {:?}", duration);
        process_batch(&mut reader, &chtm, args.chunk_dir.clone(), args.batch_size)?;
    } else {
        let config = HashConfig::<u32>::from(&args.index_filename.join("hash_config.k2d"))?;
        let parition = hash_files.len();
        let chtm = CHPage::from(
            config,
            &hash_files[page_index],
            &hash_files[(page_index + 1) % parition],
        )?;
        // 计算持续时间
        let duration = start.elapsed();
        // 打印运行时间
        println!("load table took: {:?}", duration);
        process_batch(&mut reader, &chtm, args.chunk_dir.clone(), args.batch_size)?;
    }

    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();

    let chunk_files = find_and_sort_files(&args.chunk_dir, "sample", ".k2", 0)?;

    let hash_files = if args.index_filename.is_dir() {
        let hash_prefix = "hash";
        find_and_sort_files(&args.index_filename, hash_prefix, ".k2d", 0)?
    } else {
        vec![args.index_filename.clone()]
    };
    println!("hash_files {:?}", hash_files);
    // 开始计时
    let start = Instant::now();
    println!("start...");
    for chunk_file in chunk_files {
        println!("chunk_file {:?}", chunk_file);
        process_chunk_file(&args, chunk_file, &hash_files)?;
    }
    // 计算持续时间
    let duration = start.elapsed();
    // 打印运行时间
    println!("squid took: {:?}", duration);

    Ok(())
}
