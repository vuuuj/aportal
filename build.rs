//! 构建脚本: 用与主程序相同的算法渲染 APortal 图标,
//! 打包成 .ico (经典 DIB 格式, 无 PNG 依赖), 再嵌入 exe 资源。
//! 生成的 .ico 会作为 exe 的程序图标 (资源管理器/任务栏可见)。

#[path = "src/app_icon.rs"]
mod app_icon;

fn main() {
    // 注意: rc.exe 无法处理含中文的路径; OUT_DIR 通常位于中文工作区下,
    // 因此不用 OUT_DIR, 固定在系统临时目录 (用户目录需为 ASCII, 本项目如此)。
    let out_dir = std::env::temp_dir().join("aportal_icon");
    std::fs::create_dir_all(&out_dir).expect("创建临时图标目录失败");

    // 1. 渲染多尺寸图标并打包成 .ico
    let ico_path = out_dir.join("aportal.ico");
    let ico_data = pack_ico(&[16, 24, 32, 48, 64]);
    std::fs::write(&ico_path, &ico_data).expect("写 aportal.ico 失败");

    // 2. 写 .rc 资源描述文件 (用绝对路径, rc 编译器可定位 ico)
    //    含 VERSIONINFO 版本资源: 右键 exe → 属性 → 详细信息可见;
    //    版本号单源 = Cargo.toml (CARGO_PKG_VERSION)。
    let ico_abs = ico_path.to_string_lossy().replace('\\', "/");
    let ver = env!("CARGO_PKG_VERSION"); // e.g. "0.0.8"
    let parts: Vec<u32> = ver
        .split('.')
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    let (v1, v2, v3, v4) = match parts.as_slice() {
        [a, b, c] => (*a, *b, *c, 0),
        [a, b, c, d] => (*a, *b, *c, *d),
        [a, b] => (*a, *b, 0, 0),
        [a] => (*a, 0, 0, 0),
        _ => (0, 0, 0, 0),
    };
    let rc = format!(
        "#include <winver.h>\n\
         1 ICON \"{ico}\"\n\
         1 VERSIONINFO\n\
         FILEVERSION {v1},{v2},{v3},{v4}\n\
         PRODUCTVERSION {v1},{v2},{v3},{v4}\n\
         FILEOS VOS_NT_WINDOWS32\n\
         FILETYPE VFT_APP\n\
         BEGIN\n\
             BLOCK \"StringFileInfo\"\n\
             BEGIN\n\
                 BLOCK \"040904b0\"\n\
                 BEGIN\n\
                     VALUE \"FileDescription\", \"屏幕区域重组显示工具\"\n\
                     VALUE \"FileVersion\", \"{v1}.{v2}.{v3}.{v4}\"\n\
                     VALUE \"InternalName\", \"APortal\"\n\
                     VALUE \"OriginalFilename\", \"APortal.exe\"\n\
                     VALUE \"ProductName\", \"APortal\"\n\
                     VALUE \"ProductVersion\", \"{ver}\"\n\
                 END\n\
             END\n\
             BLOCK \"VarFileInfo\"\n\
             BEGIN\n\
                 VALUE \"Translation\", 0x409, 1200\n\
             END\n\
         END\n",
        ico = ico_abs,
    );
    // rc.exe 对 UTF-8 无 BOM 的中文支持不可靠, 用 UTF-16 LE + BOM 写整个 .rc
    let mut rc_bytes = vec![0xFF, 0xFE];
    for unit in rc.encode_utf16() {
        rc_bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let rc_path = out_dir.join("aportal.rc");
    std::fs::write(&rc_path, rc_bytes).expect("写 aportal.rc 失败");

    // 3. 编译并嵌入 exe
    embed_resource::compile(rc_path.to_str().unwrap(), embed_resource::NONE);

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/app_icon.rs");
}

/// 把多个尺寸的 BGRA 渲染结果打包成 .ico 文件
fn pack_ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&s| encode_dib_entry(s)).collect();

    let count = sizes.len() as u16;
    let mut data = Vec::new();
    // ICONDIR
    data.extend_from_slice(&0u16.to_le_bytes()); // reserved
    data.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    data.extend_from_slice(&count.to_le_bytes());
    // ICONDIRENTRY (16 bytes each)
    let mut offset = 6u32 + 16 * count as u32;
    for (i, img) in images.iter().enumerate() {
        let w = sizes[i];
        data.push(if w >= 256 { 0 } else { w as u8 }); // width (0 = 256)
        data.push(if w >= 256 { 0 } else { w as u8 }); // height
        data.push(0); // color count
        data.push(0); // reserved
        data.extend_from_slice(&1u16.to_le_bytes()); // planes
        data.extend_from_slice(&32u16.to_le_bytes()); // bit count
        data.extend_from_slice(&(img.len() as u32).to_le_bytes()); // bytes in res
        data.extend_from_slice(&offset.to_le_bytes()); // image offset
        offset += img.len() as u32;
    }
    for img in &images {
        data.extend_from_slice(img);
    }
    data
}

/// 单个尺寸: BITMAPINFOHEADER + 自下而上 BGRA 像素 + AND mask(全 0)
fn encode_dib_entry(size: u32) -> Vec<u8> {
    let top_down = app_icon::render_portal(size); // 自上而下 BGRA
    let mut dib = Vec::new();

    // BITMAPINFOHEADER (40 bytes)
    dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
    dib.extend_from_slice(&size.to_le_bytes()); // biWidth
    dib.extend_from_slice(&(size * 2).to_le_bytes()); // biHeight = 2x (含 mask)
    dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    dib.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    dib.extend_from_slice(&0u32.to_le_bytes()); // biCompression (BI_RGB)
    dib.extend_from_slice(&0u32.to_le_bytes()); // biSizeImage
    dib.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    dib.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    dib.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    dib.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    // 像素: DIB 是自下而上, 需要上下翻转
    let row_bytes = (size * 4) as usize;
    for y in (0..size).rev() {
        let start = (y * size * 4) as usize;
        dib.extend_from_slice(&top_down[start..start + row_bytes]);
    }

    // AND mask: 每行 1bit/像素, 32bit 对齐, 全 0 (不遮罩)
    let mask_row = (size as usize).div_ceil(32) * 4;
    dib.resize(dib.len() + mask_row * size as usize, 0);

    dib
}