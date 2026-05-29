# toon-evals — Documentation

The eval harness measures the **quality** of the toon-mcp compression pipeline
(as opposed to `toon-mcp-bench`, which measures latency). It drives a local,
OpenAI-compatible `llama-server` to synthesize realistic structured payloads and
to act as a reading subject, then scores the pipeline deterministically.

```mermaid
flowchart LR
    subgraph model["llama-server (local)"]
        LM[gguf model]
    end

    G["generate<br/>(LLM)"] -->|corpus.jsonl| P["pipeline<br/>(deterministic)"]
    G -->|corpus.jsonl| C["comprehend<br/>(LLM)"]
    P -->|pipeline.jsonl| R["report"]
    C -->|comprehension.jsonl| R
    R --> OUT["report.md<br/>summary.json"]

    G -. "chat /v1/chat/completions" .-> LM
    C -. "chat + answers" .-> LM
    P -. "tokens /tokenize" .-> LM

    style G fill:#cce5ff,stroke:#004085
    style C fill:#cce5ff,stroke:#004085
    style P fill:#d4edda,stroke:#28a745
    style R fill:#fff3cd,stroke:#ffc107
```

Blue stages require the model server; the green stage is deterministic and only
contacts the server for exact token counts (it degrades gracefully when the
server is down).

## Read in this order

| Doc                                | What it covers                                                                                              |
| ---------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| [usage.md](usage.md)               | Commands (`just` + raw `cargo`), the `serve.sh` server manager, and llama.cpp HTTP interfacing              |
| [architecture.md](architecture.md) | Crate layout, module dependencies, and end-to-end data flow                                                 |
| [methodology.md](methodology.md)   | How corpora are generated, how the pipeline is scored, and how comprehension questions are built and graded |
| [metrics.md](metrics.md)           | The four metric axes, their formulas, and how to read `report.md` / `summary.json`                          |

For a quick start, see the harness [README](../README.md).

## The four metrics at a glance

| Axis                        | Question it answers                                                          | Stage      |
| --------------------------- | ---------------------------------------------------------------------------- | ---------- |
| **Token savings**           | How many context tokens does TOON actually save under the model's tokenizer? | pipeline   |
| **Round-trip fidelity**     | Does `encode → decode` reproduce the original value losslessly?              | pipeline   |
| **Classification accuracy** | Does the classifier assign the shape the data really is?                     | pipeline   |
| **Comprehension parity**    | Can the model answer questions about TOON as accurately as about JSON?       | comprehend |
