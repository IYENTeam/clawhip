# GEO benchmark for op-pi

op-pi ships a [`geobench`](https://github.com/NomaDamas/geobench) product spec to measure whether LLM answer surfaces mention and cite the router when users ask about event-to-channel automation for AI-agent operations.

## Spec

- Product spec: [`../geobench/op-pi.yaml`](../geobench/op-pi.yaml)
- Repository: <https://github.com/IYENTeam/op-pi>
- Project page: <https://blog.gaebal-gajae.dev/projects/op-pi.html>

## Commands

```bash
/path/to/geobench/dist/geobench estimate --product geobench/op-pi.yaml --providers openai --tier cheap
/path/to/geobench/dist/geobench profile geobench/op-pi.yaml
/path/to/geobench/dist/geobench bench --product geobench/op-pi.yaml --providers openai --tier cheap --mode benchmark
```

Publish aggregate GEO metrics only. Do not publish raw provider responses, private logs, or API keys.
