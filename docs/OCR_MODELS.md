# PP-OCRv4 模型来源与许可

LaterScreen 只下载并缓存模型，不把权重打进程序、不执行上游 Python/脚本、不接受任意模型 URL。下载器在 `crates/ocr/src/paddle_models.rs` 固定文件名、大小和 SHA-256，校验通过后才发布到缓存目录。

## 固定文件

| 文件 | 来源 | 大小（字节） | SHA-256 |
|---|---|---:|---|
| `ch_PP-OCRv4_det_mobile.onnx` | [RapidAI/RapidOCR ModelScope](https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master/onnx/PP-OCRv4/det/ch_PP-OCRv4_det_mobile.onnx) | 4,745,517 | `d2a7720d45a54257208b1e13e36a8479894cb74155a5efe29462512d42f49da9` |
| `ch_PP-OCRv4_rec_mobile.onnx` | [RapidAI/RapidOCR ModelScope](https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master/onnx/PP-OCRv4/rec/ch_PP-OCRv4_rec_mobile.onnx) | 10,857,958 | `48fc40f24f6d2a207a2b1091d3437eb3cc3eb6b676dc3ef9c37384005483683b` |
| `ppocr_keys_v1.txt` | [RapidAI/RapidOCR ModelScope](https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master/paddle/PP-OCRv4/rec/ch_PP-OCRv4_rec_mobile/ppocr_keys_v1.txt) | 26,249 | `28b2362ad4ab2dc38769aa72feb535e3a9ddb3fd2a7585a05920e6393b1dc7f7` |

合计 **15,629,724 字节**。版本目录为 `v1`；更换模型组合时应升级目录和 manifest，不覆盖旧文件。

## 许可边界

- LaterScreen 主程序是 MIT；上表模型不是本仓库原创，也不因下载器存在而变成 MIT。
- 推理实现参考 [PaddleOCR](https://github.com/PaddlePaddle/PaddleOCR) / [RapidOCR](https://github.com/RapidAI/RapidOCR) 的公开实现；发布前应随模型分发或在产品的第三方声明中保留对应 Apache-2.0 许可文本与版权/NOTICE 信息。
- [PaddleOCR LICENSE](https://github.com/PaddlePaddle/PaddleOCR/blob/main/LICENSE)、[RapidOCR LICENSE](https://github.com/RapidAI/RapidOCR/blob/main/LICENSE) 和 [PaddleOCR 模型目录](https://github.com/PaddlePaddle/PaddleOCR/blob/main/docs/version2.x/ppocr/model_list.en.md) 是核查入口。模型文件的具体许可/归属以其上游仓库和发布页当前说明为准；不要把模型再打包到 LaterScreen 的安装包或 release 资产中。
- 下载模型前由用户显式点击或显式选择引擎；识别图片只在本地进入 tract，不上传给 ModelScope 或其它服务。

## 缓存与离线安装

默认缓存：`$XDG_CACHE_HOME/lscreen/ppocr/v1/`（XDG_CACHE_HOME 为绝对路径时），否则 `~/.cache/lscreen/ppocr/v1/`；Windows 使用 HOME/USERPROFILE 下 `.cache/lscreen/ppocr/v1/`。可在设置中下载，也可把三份**已核验的固定文件**放入该目录后点「刷新本地状态」。失败下载留下的 `.part` 不算模型，缺失/坏文件会在加载时明确报错。
