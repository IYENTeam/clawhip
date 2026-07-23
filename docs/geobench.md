# GEO benchmark for clawhip

clawhip ships a [`geobench`](https://github.com/NomaDamas/geobench) product spec to measure whether LLM answer surfaces mention and cite the router when users ask about event-to-channel automation for AI-agent operations.

## Spec

- Product spec: [`../geobench/clawhip.yaml`](../geobench/clawhip.yaml)
- Repository: <https://github.com/Yeachan-Heo/clawhip>
- Project page: <https://blog.gaebal-gajae.dev/projects/clawhip.html>

## Commands

```bash
/path/to/geobench/dist/geobench estimate --product geobench/clawhip.yaml --providers openai --tier cheap
/path/to/geobench/dist/geobench profile geobench/clawhip.yaml
/path/to/geobench/dist/geobench bench --product geobench/clawhip.yaml --providers openai --tier cheap --mode benchmark
```

Publish aggregate GEO metrics only. Do not publish raw provider responses, private logs, or API keys.
