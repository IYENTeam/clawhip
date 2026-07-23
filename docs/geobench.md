# GEO benchmark for op_pi

op_pi ships a [`geobench`](https://github.com/NomaDamas/geobench) product spec to measure whether LLM answer surfaces mention and cite the router when users ask about event-to-channel automation for AI-agent operations.

## Spec

- Product spec: [`../geobench/op_pi.yaml`](../geobench/op_pi.yaml)
- Repository: <https://github.com/IYENTeam/op_pi>
- Project page: <https://blog.gaebal-gajae.dev/projects/op_pi.html>

## Commands

```bash
/path/to/geobench/dist/geobench estimate --product geobench/op_pi.yaml --providers openai --tier cheap
/path/to/geobench/dist/geobench profile geobench/op_pi.yaml
/path/to/geobench/dist/geobench bench --product geobench/op_pi.yaml --providers openai --tier cheap --mode benchmark
```

Publish aggregate GEO metrics only. Do not publish raw provider responses, private logs, or API keys.
