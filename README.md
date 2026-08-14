# 屏幕区域重组显示工具 (APortal)

实时捕获屏幕上的多个矩形区域，缩放/换位后绘制到一个置顶、透明、点击穿透的 Overlay 窗口上，并可叠加自定义的框/底/图/字 UI。外部独立进程，只读屏幕像素，不注入、不 Hook、不改游戏内存。支持检测键盘/手柄输入，自动切换到对应的配置文件。

## 原理

1. `IDXGIOutputDuplication` 获取桌面帧，仅对配置区域做 GPU 裁剪与缩放
2. 全部区域合并到单个 staging texture 后一次回读 CPU
3. CPU 侧合成到 BGRA 预乘 alpha 缓冲（区域画面 + UI 元素）
4. `UpdateLayeredWindow` 一次性上传，由 DWM 合成到屏幕

## 构建

环境要求：Windows + Rust MSVC 工具链（需要 `rc.exe` 嵌入图标/版本资源）。

```bash
cargo build --release
```

产物：`target/release/APortal.exe`

## 使用

1. 把 `APortal.exe` 放到任意目录（`settings.yml`、区域配置 `*.yml`、`PNG/` 图片、`lang/` 语言文件都放在 exe 同目录）
2. 双击运行，托盘图标出现
3. 右键托盘 → 新建配置 → 框选屏幕区域 → 拖拽排布 → 保存
4. 在托盘菜单勾选启用配置，立即生效

### 配置格式（示例见 `示例.yml`）

```yml
settings:
  global_opacity: 0.55        # 全局透明度 (区域/元素未单独设置时生效; 均可单独设 opacity 覆盖)
capture_regions:
- id: skill_1                  # 捕获区域
  source: {x: 100, 'y': 100, w: 60, h: 60}   # 屏幕绝对坐标
  display: {x: 200, 'y': 800, opacity: 0.8}  # Overlay 内坐标, 可选 w/h 缩放、z 排序、rotate 旋转、opacity 单独透明度
custom_ui:
- type: image                  # 自定义 UI: frame / background / image / text
  path: X.png
  geometry: {x: 220, 'y': 740, z: 10, opacity: 0.6}   # opacity 可选, 不写则继承全局
```

### 全局设置（`settings.yml`）

- `fps`：目标帧率 30/60/120/240
- `enabled_configs`：启用的区域配置列表
- `input_auto_switch` + `keyboard_configs` / `controller_configs`：按输入设备自动切换配置
- `lang`：`zh` / `en`
- `log_enabled`：是否写 `log/log.txt`（调试用）

## 许可

[MIT License](LICENSE) —— 可自由使用、修改、分发（含商用），仅需保留版权声明。
