# Hardware Analysis: Autonomous AI Agent Deployment

**Date:** 2026-03-07
**Hardware:** Mac Mini M4 (32GB), Jetson Nano Orin (8GB), Main PC (128GB/4090)
**Target Model:** Qwen 3.5 A3B 35B MoE

---

## 1. Qwen 3.5 35B A3B on Mac Mini M4

### 1.1 RAM Usage by Quantization

| Quantization | GGUF Size (approx) | RAM at Load | RAM w/ 32K Context | Fits 32GB? |
| ------------ | ------------------ | ----------- | ------------------ | ---------- |
| Q4_K_M       | ~20-22 GB          | ~22 GB      | ~24-26 GB          | Yes, tight |
| Q5_K_M       | ~25-27 GB          | ~27 GB      | ~29-31 GB          | Barely     |
| Q8_0         | ~37 GB             | ~37 GB      | ~40+ GB            | No         |
| FP16         | ~70 GB             | ~70 GB      | N/A                | No         |

**Key insight:** The MoE architecture means only ~3B parameters are active per token, but all 35B parameters must reside in memory. The model weight storage dominates RAM, not compute.

**Recommendation:** Q4_K_M is the only viable quantization for 32GB. Q5_K_M is theoretically possible but leaves almost no headroom for tools, OS, or context.

Sources: [Unsloth Qwen3.5 Guide](https://unsloth.ai/docs/models/qwen3.5), [InsiderLLM Local Guide](https://insiderllm.com/guides/qwen-3-5-local-guide/), [Unsloth GGUF on HuggingFace](https://huggingface.co/unsloth/Qwen3.5-35B-A3B-GGUF)

### 1.2 Inference Speed

| Framework | Estimated tok/s (M4 base) | Notes                                   |
| --------- | ------------------------- | --------------------------------------- |
| MLX       | ~40-50 tok/s              | Native Apple Silicon optimization       |
| llama.cpp | ~25-35 tok/s              | Solid but slower than MLX               |
| Ollama    | ~20-30 tok/s              | Uses llama.cpp backend, slight overhead |

**Context:** M4 Max benchmarks show ~35 tok/s via Ollama, ~60-70 tok/s via MLX. The base M4 has fewer GPU cores (10 vs 40 on M4 Max) and lower memory bandwidth (~100 GB/s vs ~273 GB/s on M4 Pro, ~546 GB/s on M4 Max). Expect roughly 40-60% of M4 Max performance.

MLX is consistently 21-87% faster than llama.cpp on Apple Silicon. The gap has widened significantly in 2025-2026, reversing earlier parity. MLX achieves ~230 tok/s on small models, llama.cpp ~150 tok/s, Ollama 20-40 tok/s (general benchmarks, not Qwen 3.5 specific).

**Critical caveat:** The base M4 has ~100 GB/s memory bandwidth. LLM inference is memory-bandwidth-bound. This is the bottleneck, not compute. The M4 Pro (~273 GB/s) and M4 Max (~546 GB/s) would be dramatically faster.

Sources: [MLX vs Ollama Benchmarks](https://insiderllm.com/guides/qwen35-mac-mlx-vs-ollama/), [llama.cpp Apple Silicon Discussion](https://github.com/ggml-org/llama.cpp/discussions/4167), [MLX vs llama.cpp Study (arXiv:2511.05502)](https://arxiv.org/abs/2511.05502), [Ollama Qwen3.5 Tag](https://ollama.com/library/qwen3.5:35b-a3b-q4_K_M)

### 1.3 Headroom After Loading

With Q4_K_M (~22 GB loaded):

- **32GB total - 22GB model = ~10 GB remaining**
- macOS system overhead: ~3-4 GB
- Usable headroom: **~6-7 GB**

This is enough for:

- Tool processes (shell commands, file ops): ~0.5-1 GB
- Memory/vector DB (e.g., ChromaDB, SQLite): ~0.5-1 GB
- Python runtime for agent logic: ~0.5-1 GB
- Remaining buffer: ~3-5 GB

**Warning:** macOS GPU allocation defaults to ~66% of unified memory (~21 GB on 32GB). This can be tuned, but running the model at Q4_K_M will consume nearly all GPU-allocated memory.

### 1.4 Running a Second Model Simultaneously

With ~6-7 GB headroom after the main model:

| Auxiliary Model         | RAM Required | Feasible?              |
| ----------------------- | ------------ | ---------------------- |
| nomic-embed-text (137M) | ~0.3 GB      | Yes                    |
| all-MiniLM-L6 (22M)     | ~0.1 GB      | Yes                    |
| SmolLM2 1.7B Q4         | ~1.2 GB      | Yes                    |
| Phi-4-mini 3.8B Q4      | ~2.5 GB      | Tight but possible     |
| Qwen2.5 3B Q4           | ~2 GB        | Yes                    |
| Qwen2.5 7B Q4           | ~4.5 GB      | Risky, memory pressure |

**Verdict:** An embedding model + a small (1-3B) auxiliary model is feasible. A 7B auxiliary model would push into memory pressure territory.

### 1.5 Framework Recommendation

**Use MLX for the main model.** It is purpose-built for Apple Silicon and consistently outperforms llama.cpp/Ollama by 20-87%. The tradeoff is less ecosystem compatibility (no OpenAI-compatible API by default), but `mlx-lm` and `vllm-mlx` provide serving capabilities.

For the auxiliary model, Ollama is fine since it provides a clean API and the performance difference matters less for small models.

---

## 2. Alternative Models for 32GB M4

### 2.1 Qwen 3 30B A3B vs Qwen 3.5 35B A3B

| Dimension            | Qwen 3 30B A3B                  | Qwen 3.5 35B A3B                  |
| -------------------- | ------------------------------- | --------------------------------- |
| Total params         | 30B                             | 35B                               |
| Active params        | ~3B                             | ~3B                               |
| Speed (relative)     | 32% faster                      | Baseline                          |
| Long-context quality | 21% quality drop at tail of 32K | Flat performance across context   |
| Reasoning            | Vague ("probably", "maybe")     | Step-by-step structured reasoning |
| Multimodal           | Text only                       | Vision projector included         |
| RAM (Q4_K_M)         | ~18-20 GB                       | ~20-22 GB                         |

**Verdict:** Qwen 3.5 35B A3B is strictly better in quality. The 2 GB extra RAM is worth it. The 32% speed loss is acceptable for an autonomous agent where quality matters more than interactive speed.

Sources: [Qwen3.5 vs Qwen3 Comparison](https://aihaberleri.org/en/news/comparison-of-qwen35-35b-a3b-and-qwen3-30b-a3b-did-speed-or-quality-win-on-the-rtx-5090), [VentureBeat Coverage](https://venturebeat.com/technology/alibabas-new-open-source-qwen3-5-medium-models-offer-sonnet-4-5-performance)

### 2.2 Other Models That Fit 32GB

| Model                       | Params    | Q4_K_M Size | Quality Tier                | Notes                         |
| --------------------------- | --------- | ----------- | --------------------------- | ----------------------------- |
| **Qwen 3.5 35B A3B**        | 35B MoE   | ~22 GB      | Top tier (Sonnet 4.5 level) | Best bang for buck            |
| **Qwen 3.5 27B**            | 27B dense | ~16 GB      | Very good                   | Dense, simpler, more headroom |
| **DeepSeek R1 14B distill** | 14B       | ~8-9 GB     | Good reasoning              | Strong chain-of-thought       |
| **DeepSeek R1 32B distill** | 32B       | ~19 GB      | Very good reasoning         | Fits with headroom            |
| **Phi-4 14B**               | 14B       | ~8-9 GB     | Good for size               | Microsoft, strong coding      |
| **Phi-4-mini 3.8B**         | 3.8B      | ~2.5 GB     | Auxiliary tier              | Good for tool use             |
| **Mixtral 8x7B**            | 47B MoE   | ~26-28 GB   | Good                        | Tight fit, older              |
| **Llama 3.2 3B**            | 3B        | ~2 GB       | Auxiliary tier              | Meta, good general            |
| **Gemma 3 4B**              | 4B        | ~2.5 GB     | Auxiliary tier              | Google, good reasoning        |

**Top recommendation for 32GB:** Qwen 3.5 35B A3B (Q4_K_M). It reportedly matches Sonnet 4.5 performance while fitting in 32GB with headroom for tools.

Sources: [Best LLMs for Mac](https://modelfit.io/guides/best-llm-for-macbook/), [Best Local LLMs Apple Silicon](https://apxml.com/posts/best-local-llm-apple-silicon-mac), [DeepSeek R1 Mac Guide](https://apxml.com/posts/deepseek-system-requirements-mac-os-guide), [Best Mac Mini for LLMs](https://blog.starmorph.com/blog/best-mac-mini-for-local-llms)

### 2.3 llmfit Tool

[llmfit](https://github.com/AlexsJones/llmfit) is a CLI/TUI tool that right-sizes LLM models to your hardware.

**What it does:**

- Detects your system's RAM, CPU, and GPU
- Scores 206+ models across quality, speed, fit, and context dimensions
- Supports MoE architectures, dynamic quantization selection, speed estimation
- Integrates with Ollama, llama.cpp, and MLX as runtime providers
- Available via Homebrew (`brew install llmfit`) and AUR

**How it helps:** Run it on the Mac Mini M4 to get a hardware-specific ranking of which models and quantizations will perform best. Saves manual calculation of what fits.

Sources: [llmfit GitHub](https://github.com/AlexsJones/llmfit), [llmfit Homebrew](https://formulae.brew.sh/formula/llmfit)

---

## 3. Jetson Nano Orin 8GB

### 3.1 Inference Capabilities

| Model Size                | Quantization | Estimated tok/s | Feasible?         |
| ------------------------- | ------------ | --------------- | ----------------- |
| 1B (Qwen2.5, SmolLM2)     | INT4         | 40-55 tok/s     | Yes               |
| 3B (Qwen2.5, Gemma, VILA) | INT4         | 28-40 tok/s     | Yes               |
| 4B (Gemma 3)              | INT4         | 20-30 tok/s     | Yes, near limit   |
| 7B (Llama 3.2)            | INT4         | 10-15 tok/s     | Possible but slow |
| 8B+                       | Any          | <10 tok/s       | Not practical     |

The Jetson Orin Nano 8GB supports models up to ~4B parameters comfortably with INT4 quantization. Frameworks supported: Ollama, llama.cpp, TensorRT-LLM, MLC, HuggingFace Transformers.

Sources: [NVIDIA Jetson Edge AI Blog](https://developer.nvidia.com/blog/getting-started-with-edge-ai-on-nvidia-jetson-llms-vlms-and-foundation-models-for-robotics), [On-Device LLM Inference](https://genaiprotos.medium.com/on-device-llm-inference-on-nvidia-jetson-orin-nano-0e7c7066d062), [TensorRT-LLM on Jetson](https://collabnix.com/running-llms-with-tensorrt-llm-on-nvidia-jetson-orin-nano-super/)

### 3.2 Fine-Tuning / LoRA

- **LoRA fine-tuning of 1-3B models:** Feasible with QLoRA (4-bit base + LoRA adapters). BitsAndBytes + PEFT libraries support Jetson.
- **EdgeLoRA** (arXiv:2507.01438): Research demonstrates efficient multi-tenant LoRA serving on Jetson Orin Nano, serving thousands of LoRA adapters for Llama3.2-3B and OpenELM-1.1B.
- **NVIDIA NeMo** provides recipes for alignment and specialization of SLMs on Jetson.
- **Realistic training scope:** Fine-tune a 1-3B model with LoRA rank 8-16. Full fine-tuning is not feasible at 8GB.

Sources: [EdgeLoRA Paper](https://arxiv.org/html/2507.01438), [Fine-Tuning on Jetson AGX](https://www.hackster.io/shahizat/fine-tuning-llms-using-nvidia-jetson-agx-orin-b17c4d), [JetPack 6.2 Super Mode](https://developer.nvidia.com/blog/nvidia-jetpack-6-2-brings-super-mode-to-nvidia-jetson-orin-nano-and-jetson-orin-nx-modules/)

### 3.3 Power and Thermal

| Metric                             | Value                                                      |
| ---------------------------------- | ---------------------------------------------------------- |
| Idle power                         | ~7 W                                                       |
| Full GPU load                      | 15-20 W                                                    |
| TDP budget (performance mode)      | 25 W                                                       |
| SOC consumption at max             | 20.2 W                                                     |
| Safe operating temp                | < 80 C                                                     |
| Thermal throttling                 | Hardware-based clock throttling when exceeding power limit |
| 24/7 feasibility                   | Yes, with active cooling (small fan)                       |
| Annual power cost (24/7, ~15W avg) | ~$13/year at $0.10/kWh                                     |

Sources: [NVIDIA Power Docs](https://docs.nvidia.com/jetson/archives/r36.4.3/DeveloperGuide/SD/PlatformPowerAndPerformance/JetsonOrinNanoSeriesJetsonOrinNxSeriesAndJetsonAgxOrinSeries.html), [NVIDIA Forum Power Discussion](https://forums.developer.nvidia.com/t/power-consumption-nvidia-jetson-orin-nano-developer-kit-8gb/330902)

### 3.4 Realistic Use Cases

1. **Embedding generation:** Run nomic-embed-text or all-MiniLM-L6 for vector search. Low RAM, fast, always-on.
2. **Small model inference:** Run a 1-3B model for quick classification, summarization, or tool-use decisions.
3. **LoRA fine-tuning:** Specialize a 1-3B model on domain-specific data (overnight batch jobs).
4. **Sensor/data preprocessing:** If connected to physical sensors, preprocess before sending to Mac Mini.
5. **Embedding cache/index:** Maintain a local vector DB with FAISS or ChromaDB.

### 3.5 Is the Jetson Worth It?

**Arguments for:**

- Offloads embedding and auxiliary model work from Mac Mini, freeing RAM for the main model
- 7-25W power envelope makes it viable for 24/7
- CUDA support for optimized inference (TensorRT-LLM)
- Can run specialized fine-tuned models independently

**Arguments against:**

- The Mac Mini has enough headroom (~6-7 GB) to run embeddings + a small model itself
- Network latency adds complexity and failure modes
- Another device to maintain, update, monitor
- 8GB is severely limiting for anything beyond 3B models

**Verdict:** The Jetson is worth incorporating IF you need to run the main model at maximum context length (which consumes more RAM) or if you want to do LoRA fine-tuning without interrupting the agent. For a minimal setup, the Mac Mini alone is sufficient.

---

## 4. Multi-Device Orchestration

### 4.1 Exo Framework (Distributed Inference)

[Exo](https://github.com/exo-explore/exo) is the most relevant framework for heterogeneous distributed inference.

**Architecture:**

- Pipeline parallel inference: splits model into "shards" (contiguous layer slices)
- Each shard assigned to a different device (GPU, CPU, NPU, or separate machine)
- Devices connected over network (Ethernet, WiFi)

**Compatibility:**

- OpenAI Chat Completions API, Claude Messages API, Ollama API
- MLX backend for Apple Silicon, llama.cpp for other devices
- macOS app available (requires macOS Tahoe 26.2+)
- Community reports of 3-node Mac Mini clusters

**Limitation for Mac + Jetson:** Exo primarily targets homogeneous Apple Silicon clusters. Jetson support via llama.cpp backend is possible but less tested. The heterogeneous nature (ARM64 Apple vs ARM64 Jetson with CUDA) adds complexity.

Sources: [Exo GitHub](https://github.com/exo-explore/exo), [Exo Deep Dive](https://medium.com/@leif.markthaler/deep-dive-exo-distributed-ai-inference-on-consumer-hardware-068e341d8e3c), [Exo Benchmarks](https://blog.exolabs.net/day-1/)

### 4.2 Hive (Distributed Ollama)

[Hive](https://www.sciencedirect.com/science/article/pii/S2352711025001505) is an open-source framework for distributed Ollama inference.

- **HiveCore:** Central proxy that routes requests
- **HiveNode:** Lightweight worker agent on each machine running Ollama
- No public network exposure required
- Integrates fragmented compute resources

This is simpler than Exo but only distributes requests, not model layers. Each node must be able to run the model independently.

### 4.3 Ollama Remote Mode

Ollama can serve over HTTP natively:

```
# On Jetson (embedding server)
OLLAMA_HOST=0.0.0.0:11434 ollama serve

# On Mac Mini (call Jetson for embeddings)
curl http://<jetson-ip>:11434/api/embeddings -d '{"model":"nomic-embed-text","prompt":"..."}'
```

This is the simplest approach: Mac Mini runs the main model locally, calls Jetson's Ollama API for embeddings. No framework overhead.

### 4.4 Network Considerations

| Connection           | Latency | Bandwidth                         | Suitability                  |
| -------------------- | ------- | --------------------------------- | ---------------------------- |
| Gigabit Ethernet     | <1 ms   | 125 MB/s                          | Best for embedding API calls |
| USB Ethernet adapter | <1 ms   | 125 MB/s                          | Same as Ethernet             |
| WiFi 6               | 1-5 ms  | 50-100 MB/s                       | Acceptable for API calls     |
| USB direct (RNDIS)   | <0.5 ms | 480 MB/s (USB 2) / 5 GB/s (USB 3) | Lowest latency               |

For API-level communication (sending text, receiving embeddings), even WiFi is fine. Distributed model sharding (Exo-style) benefits from Ethernet or USB direct.

### 4.5 Recommended Architecture

```
Mac Mini M4 (32GB)                    Jetson Orin Nano (8GB)
+---------------------------+         +------------------------+
| Main Agent Process        |         | Ollama Server          |
| - Qwen 3.5 35B A3B (MLX) |  HTTP   | - nomic-embed-text     |
| - Agent logic (Python)    | ------> | - SmolLM2 1.7B (tools) |
| - Memory DB (SQLite)      |  GigE   | - LoRA fine-tune jobs   |
| - Tool execution          |         |   (batch, overnight)   |
+---------------------------+         +------------------------+
```

---

## 5. Mac Mini M4 as 24/7 Server

### 5.1 Process Supervision with launchd

macOS uses `launchd` for process management (equivalent to systemd on Linux).

**Key configuration for always-on agent:**

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "...">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.agent.backbone</string>
    <key>ProgramArguments</key>
    <array>
        <string>/path/to/agent</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>Crashed</key>
        <true/>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>/var/log/agent.log</string>
    <key>StandardErrorPath</key>
    <string>/var/log/agent.err</string>
</dict>
</plist>
```

- `KeepAlive.Crashed = true`: Restarts on crash
- `KeepAlive.SuccessfulExit = false`: Restarts if process exits with 0
- `RunAtLoad = true`: Starts at boot/login
- Use `LaunchDaemons` (runs as root, no GUI) vs `LaunchAgents` (runs as user, GUI access)

For headless operation, `LaunchDaemons` is preferred. If GPU acceleration requires WindowServer, use `LaunchAgents` + HDMI dummy plug.

Sources: [launchd Tutorial](https://www.launchd.info/), [macOS App Auto-Restart](https://notes.alinpanaitiu.com/Restarting-macOS-apps-automatically-on-crash), [Apple launchd Docs](https://support.apple.com/guide/terminal/script-management-with-launchd-apdc6c1077b-5d5d-4d35-9c19-60f2397b2369/mac)

### 5.2 Thermal Performance

| Metric                             | Value                         |
| ---------------------------------- | ----------------------------- |
| Idle temp                          | ~30-35 C                      |
| Sustained inference temp           | ~42-50 C                      |
| TDP                                | ~25-30 W peak                 |
| Idle power                         | 4-7 W                         |
| Peak power                         | 25-30 W                       |
| Throttling risk                    | Minimal with adequate airflow |
| Annual power cost (24/7, ~15W avg) | ~$13/year at $0.10/kWh        |

The M4 chip runs cool. During sustained matrix multiplication benchmarks, the M4 Pro maintained ~42 C at 18W. The base M4 draws even less power. Thermal throttling is not a practical concern for inference workloads.

**Recommendation:** Elevate the Mac Mini for airflow. Do not enclose it. No external cooling needed.

Sources: [DailyTechStack M4 Guide](https://dailytechstack.com/m4-local-ai/), [Satechi Setup Guide](https://satechi.com/blogs/news/mac-mini-m4-setup-for-local-ai-the-definitive-guide-to-storage-hubs-and-always-on-performance), [24/7 Agent on Mac Mini](https://medium.com/@akhil.reji141/the-ai-proof-infrastructure-converting-a-mac-mini-into-a-24-7-autonomous-agent-4eef4940942c)

### 5.3 SSD Endurance

| Metric                        | Value                               |
| ----------------------------- | ----------------------------------- |
| NAND type                     | TLC (Kioxia 3D NAND)                |
| Estimated TBW (512GB)         | ~300-370 TBW                        |
| Daily writes for agent (est.) | 5-20 GB/day (logs, DB, checkpoints) |
| Years to TBW at 20 GB/day     | ~41-50 years                        |

**Verdict:** SSD endurance is a non-issue. Even at 20 GB/day of writes (which is aggressive for an agent), the SSD will outlast the machine by decades.

**Mitigation for heavy write workloads:**

- Use external USB-C SSD for scratch/logs if paranoid
- Use SQLite WAL mode to reduce write amplification
- Log rotation to prevent unbounded growth

Sources: [Mac SSD Lifespan](https://createdtech.com/how-long-will-the-ssd-in-your-mac-last), [M1 SSD Concerns](https://www.macworld.com/article/338844/how-worried-should-you-be-about-your-m1-macs-ssd-lifespan.html)

### 5.4 Headless Operation

- Enable **Remote Login (SSH)** in System Preferences
- Use an **HDMI dummy plug** (~$8) if GPU acceleration requires WindowServer
- Configure **Energy Saver**: disable sleep, enable "Start up automatically after power failure"
- Use **BetterDisplay** (software) as alternative to physical dummy plug
- **Screen sharing** via built-in VNC for occasional GUI access

### 5.5 Memory Pressure Management

| Component                 | Estimated RAM      |
| ------------------------- | ------------------ |
| macOS system              | 3-4 GB             |
| Qwen 3.5 35B A3B (Q4_K_M) | 22 GB              |
| Agent process (Python)    | 0.5-1 GB           |
| Memory DB + tools         | 0.5-1 GB           |
| Embedding model           | 0.3 GB             |
| **Total**                 | **~27-28 GB**      |
| **Remaining**             | **~4-5 GB buffer** |

macOS will use compressed memory and swap before OOM-killing processes. Monitor via Activity Monitor or `memory_pressure` CLI command.

**Key setting:** GPU allocation defaults to ~66% of unified memory (~21 GB). The Qwen 3.5 model at Q4_K_M (~22 GB) slightly exceeds this default. You may need to adjust GPU allocation:

```bash
sudo sysctl iogpu.wired_limit_mb=24576  # Set to 24 GB
```

Sources: [macOS Memory Management](https://blog.greggant.com/posts/2024/07/03/macos-memory-management.html), [VRAM Tuning for LLMs](https://blog.peddals.com/en/fine-tune-vram-size-of-mac-for-llm/), [Apple Silicon LLM Limitations](https://stencel.io/posts/apple-silicon-limitations-with-usage-on-local-llm%20.html)

---

## 6. Summary and Recommendations

### Primary Configuration

| Component              | Choice                          | Rationale                                      |
| ---------------------- | ------------------------------- | ---------------------------------------------- |
| **Main model**         | Qwen 3.5 35B A3B, Q4_K_M        | Best quality in 32GB budget, Sonnet 4.5 level  |
| **Framework**          | MLX (via mlx-lm or vllm-mlx)    | 20-87% faster than llama.cpp on Apple Silicon  |
| **Embedding model**    | nomic-embed-text (137M)         | <0.3 GB, runs on Mac Mini                      |
| **Auxiliary model**    | SmolLM2 1.7B or Phi-4-mini 3.8B | Quick tool-use decisions, classification       |
| **Process management** | launchd (LaunchDaemons)         | Native macOS, crash recovery, boot persistence |
| **Memory DB**          | SQLite + vector extension       | Low overhead, ACID, no separate server         |

### Jetson Role (Optional)

| Task                         | Value                                               |
| ---------------------------- | --------------------------------------------------- |
| Embedding generation         | Offloads from Mac Mini, frees ~0.3 GB               |
| LoRA fine-tuning (overnight) | Specializes small models without interrupting agent |
| Fallback inference           | 1-3B model for degraded-mode operation              |

### Key Numbers

| Metric                                  | Value                                    |
| --------------------------------------- | ---------------------------------------- |
| Expected inference speed (main model)   | ~25-50 tok/s depending on framework      |
| Total RAM usage (agent + model + tools) | ~27-28 GB of 32 GB                       |
| Power consumption (24/7)                | ~$13/year (Mac Mini) + ~$8/year (Jetson) |
| SSD lifespan concern                    | None (decades of headroom)               |
| Thermal risk                            | Minimal (M4 runs cool)                   |

### Risk Factors

1. **Memory bandwidth is the bottleneck.** The base M4 (~100 GB/s) will be significantly slower than M4 Pro/Max. If inference speed is critical, consider upgrading to M4 Pro.
2. **Q4_K_M quality vs higher quants.** There will be some quality degradation vs Q8 or FP16, but for an autonomous agent, the speed/fit tradeoff is worth it.
3. **Context length vs RAM.** Longer contexts consume more RAM. At 32K context with Q4_K_M, you are near the ceiling. If the agent needs 100K+ context, this setup will not work.
4. **macOS GPU memory allocation.** Default ~21 GB GPU limit may need manual tuning to accommodate the ~22 GB model.

### Alternative: If Budget Allows

Upgrading to Mac Mini M4 Pro with 48GB unified RAM would:

- Allow Q5_K_M or even Q8 quantization with headroom
- Nearly triple memory bandwidth (~273 GB/s vs ~100 GB/s)
- Allow larger context windows (64K-100K)
- Make the Jetson unnecessary for most workloads
- Run a 7B auxiliary model comfortably alongside the main model
