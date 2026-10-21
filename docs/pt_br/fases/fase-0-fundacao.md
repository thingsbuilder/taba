# Fase 0 — Fundação

> Leia antes: [README das fases](README.md) (regras de processo e mapeamento de critérios) e a [spec](../spec/spec.md). Este documento recorta a spec; se houver contradição, vale a spec e o dono decide.

## Objetivo
Montar o esqueleto que todas as fases usam: API Rust, PWA instalável, base offline, CI, release e deploy automatizado.

**Entrega demonstrável em produção:** abrir `https://taba.thingsbuilder.com` no iPhone e no Android, instalar o app na tela de início, ver a tela inicial com os tokens de design e o seletor de idioma, e obter `{"unico":"ok"}` em `https://taba.thingsbuilder.com/v1/status`, servido pela API Rust lendo o Firestore. Tudo publicado pelo pipeline: tag no GitHub → release com binário e PWA → `configure.yml` baixa e instala.

## Escopo
**API (`api/`, crate e binário `taba`)**
- `axum` + `tokio`, escutando em `127.0.0.1:8080`, subcomando `serve` (spec 6.2).
- `config.rs` lendo as variáveis de ambiente (6.2.2); nesta fase só `GCP_PROJECT` é obrigatória.
- Conexão ao Firestore com a crate `firestore` (conta de serviço da VM via servidor de metadados; `FIRESTORE_EMULATOR_HOST` nos testes) (6.2, 6.3).
- `GET /v1/status` (8.1): lê `system/status` (7); documento ausente = `{"unico":"ok"}`.
- Formato de erro `{"error": "<stable_code>", "detail": {...}}` (6.2.3); logs com `tracing`, em inglês.
- Limite de corpo de 6 MB (`tower-http`) (6.2).

**PWA (`pwa/`)**
- Vite + TypeScript + Preact + `idb` + `vite-plugin-pwa` (6.1). Roteamento por `location.pathname` com `switch`.
- Manifesto instalável (nome "Taba", ícone a partir de `assets/brand/taba-app-icon.svg`), service worker com o app shell em cache.
- Design: importa `design/tokens.css`; temas claro e escuro seguindo o aparelho (design/README.md).
- Pré-requisitos do aparelho (3.0): `device_id`; no iPhone fora do modo standalone, tela "instale na tela de início" que bloqueia o resto; `navigator.storage.persist()` com aviso se `false`.
- i18n (11.1): arquivos `pwa/src/i18n/pt-BR.json`, `en.json`, `es.json` (este vazio por enquanto); detecção por `navigator.languages`; seletor "PT | EN" fixo no topo, salvo em `config.language`.
- IndexedDB `taba` versão 1 com **todos** os stores da 6.1.1 já criados (evita migração depois).
- Motor da fila (9.2), genérico: enfileirar, drenar FIFO (ao abrir, no `online`, após enfileirar, a cada 30 s), estados `pending|held|needs_action`, backoff `min(5 s × 2^attempts, 10 min) × (0,5 a 1,5)`, `Retry-After`. Sem itens reais ainda; testado com requisições falsas.
- Chip de rede "Sem sinal · N na fila" (design/README.md) e consulta a `GET /v1/status` ao abrir (3.0.4).

**CI e release (`.github/workflows/`)**
- `ci.yml` em todo push e PR: `cargo test` (com emulador do Firestore), `cargo clippy -- -D warnings`, `cargo fmt --check`; na PWA, `npm ci`, checagem de tipos, testes unitários (Vitest) e `vite build`.
- `release.yml` em tag `v*`: compila `taba` para `x86_64-unknown-linux-gnu` e empacota a PWA (`taba-pwa.tar.gz`); anexa os dois à release do GitHub.

**Infra (`infra/`)**
- `configure.yml` ganha tarefas para: baixar a release `taba_version` do GitHub; instalar o binário em `/usr/local/bin/taba`; descompactar a PWA em `/srv/taba/pwa`; instalar `taba.service` a partir de `templates/taba.service.j2` (6.4); criar swap de 1 GB; reiniciar o serviço quando o binário mudar.
- `Caddyfile.j2`: troca o placeholder por `file_server` da PWA (com fallback para `index.html`) e `reverse_proxy /v1/* 127.0.0.1:8080`, com os cabeçalhos da 6.4.
- `inventory/example/group_vars/all/vars.yml` ganha `taba_version` (e o repositório da release).
- `infra/firestore/firestore.indexes.json` (vazio nesta fase) e uma tarefa do `provision.yml` que aplica os índices e as políticas de TTL listados nele; as fases seguintes só acrescentam entradas.
- `.env.example` com as variáveis da 6.2.2 (sem valores).
- Atualizar `infra/README.md` com o fluxo de deploy (tag → release → `configure.yml -e taba_version=...`).

## Fora de escopo
Login e qualquer chamada à Unico (fase 1); qualquer tela de negócio; cifra de fotos (fase 3); idempotência no servidor (fase 2).

## Seções da spec
0, 3.0, 6 (inteira, com foco em 6.1, 6.1.1, 6.1.2, 6.2, 6.2.2, 6.2.3, 6.3, 6.4), 7 (`system/status`), 8.1 (`GET /status`), 9.2, 10.5, 11.1, 11.2, 14.

## Critérios de aceite da seção 15
- **61** — segredos (chave da Unico, `HMAC_KEY`, `THUMBNAIL_KEY`) fora do repositório e da PWA compilada; repositório sem ID de projeto GCP nem IP. Nesta fase, o CI ganha uma checagem simples (por exemplo, `grep` por `uk_live_` e por padrões de IP no repo e no `dist/`).
- **62** — rotas, coleções, campos, enums, variáveis de ambiente e mensagens de log em inglês.

Os dois são conferidos de novo pelo QA em todas as fases.

## O que a fase cria
| Tipo | Nomes |
|---|---|
| Rotas da API | `GET /v1/status` |
| Documentos | `system/status` (só leitura nesta fase) |
| Variáveis de ambiente | `GCP_PROJECT` (já gerada pelo `env.j2`); `.env.example` lista todas as da 6.2.2 |
| Telas e rotas da PWA | `/` (início), tela de instalação no iPhone, seletor PT/EN |
| Arquivos | `api/`, `pwa/`, `.github/workflows/ci.yml`, `.github/workflows/release.yml`, `infra/ansible/templates/taba.service.j2`, `infra/firestore/firestore.indexes.json`, `.env.example` |
| Infra | tarefas novas no `configure.yml` e no `provision.yml`; `Caddyfile.j2` com proxy; `taba_version` no inventário |

## Dependências
Nenhuma fase anterior. Precisa da infra já provisionada (`infra/README.md`) e do DNS do domínio apontando para a VM.

## Testes
**Unitários (obrigatórios):** leitura e validação de configuração; handler de `/v1/status` com e sem `system/status` (emulador); formato de erro; detecção de idioma (`pt-PT` → `pt-BR`, `es` → `en`, `fr` → `en`); persistência do idioma; cálculo do backoff da fila; transições de estado da fila (`2xx` remove, `5xx` reagenda, `Retry-After` respeitado, `4xx` vira `needs_action`); detecção de iOS fora do modo standalone.

**Manuais com a Unico:** nenhum (0 chamadas).

**Manuais sem a Unico:** instalar no iPhone e num Android; abrir offline depois de instalado (o app shell carrega); conferir o tema escuro.

## Riscos e pontos de atenção
- **Fontes e CSP.** `design/tokens.css` importa as fontes do Google Fonts, mas a CSP da spec (6.4) é `default-src 'self'` e o app precisa abrir offline. Recomendação: empacotar as fontes (licença OFL) junto com a PWA e não usar o `@import` remoto em produção. Registrar a escolha no PR.
- **glibc.** O binário é compilado no runner do GitHub; confira que roda no Debian 13 da VM (glibc do runner não pode ser mais nova que a da VM).
- **Memória.** A e2-micro tem 1 GB; nada de compilar na VM. Swap de 1 GB.
- **Fallback do Caddy.** Rotas da PWA (`/shelter/...`) precisam cair no `index.html`; `/v1/*` nunca.
- **Atualização do service worker.** Defina a estratégia (por exemplo, aviso "nova versão, toque para atualizar"), para o voluntário não ficar preso numa versão antiga.
- **Emulador do Firestore no CI** exige Java; fixe a versão.

## Checklist de pronto
- [ ] `api/` e `pwa/` criados conforme a spec, com testes unitários.
- [ ] CI verde (Rust e PWA) e checagem de segredos/IP ativa.
- [ ] Release `v0.x` gerada pelo `release.yml` com binário e PWA.
- [ ] `configure.yml` instala a release, o serviço systemd e a swap; Caddy com `reverse_proxy` no lugar do placeholder.
- [ ] `https://taba.thingsbuilder.com` instala no iPhone e no Android; `/v1/status` responde pela API lendo o Firestore.
- [ ] `infra/README.md` e `.env.example` atualizados.
- [ ] Critérios 61 e 62 verificados pelo QA.
- [ ] QA aprovado, PR revisado e mergeado pelo dono, deploy verificado em produção.

## Regras de processo (resumo; detalhe no README)
Senior staff engineer implementa; QA compara com a spec e com este doc item a item. Uma branch e um PR; o dono revisa; depois do merge, deploy em `taba.thingsbuilder.com`. Testes unitários em tudo; testes automáticos nunca chamam a Unico. Código, commits e PRs em inglês. Nada da instalação no repo. Problemas na documentação da Unico: registrar no arquivo interno de problemas da documentação da Unico. Pronto = QA aprovado + CI verde + PR mergeado + deploy verificado.
