# Fase 1 — Identidade do voluntário

> Leia antes: [README das fases](README.md) e a [spec](../spec/spec.md). Este documento recorta a spec; se houver contradição, vale a spec e o dono decide.

## Objetivo
Voluntário entra no Taba com o próprio rosto (Human Token com liveness, pela jornada da Unico), cria o perfil e recebe uma sessão de 7 dias. O aparelho pode ser compartilhado, com troca de operador por PIN. O operador da instância concede os papéis globais `validator` e `civil_defense` por linha de comando.

**Entrega demonstrável em produção:** no iPhone com o app instalado, tocar "Entrar", fazer a selfie na jornada, voltar ao app, preencher o perfil e ver "Olá, <nome>". Fechar e abrir o app: continua logado. Um segundo voluntário entra no mesmo aparelho, e a troca de operador por PIN funciona sem internet. `taba role grant <volunteer_id> civil_defense` na VM aparece no `GET /v1/me`.

## Escopo
**Cliente da Unico (`unico.rs`)** (5.2–5.6)
- Criar processo `humantoken` com `callbackUri = https://<DOMAIN>/return?capture=<capture_id>`; consultar processo; `extract_token` único (5.4).
- O `capture_id` é o identificador próprio (nonce) do retorno: a jornada volta para o app numa **carga nova da página** (resultado do teste no iPhone, 5.1), então a tela `/return` lê o `capture_id` da URL, sem depender de estado em memória nem de `localStorage`.
- Estados do processo e erros (5.2, 5.5): `202 capture_pending`, `422 capture_inconclusive|capture_failed|capture_canceled`, `502 unico_unexpected_state`, `429` com backoff (1, 2, 4, 8 s + jitter, até 4 tentativas → `503 unico_busy`), `500`/timeout, `401/403` → `500 unico_misconfigured`. Semáforo de 4 chamadas simultâneas.
- O tratamento do `429` de limite mensal e o modo degradado completo ficam para a fase 3; aqui basta não repetir e responder `503 unico_limit`.
- Também nesta fase: `api+humantoken` no cliente (5.3), com recusa local de imagem > 800 KB. Usado a partir da fase 3, mas testado aqui com o servidor falso.

**Identificadores e sessão**
- `HMAC_KEY` e derivação `volunteer_id = base64url(HMAC-SHA256(HMAC_KEY, token))`; o token puro nunca é gravado nem devolvido (6.3.2).
- `captures` (7): uso único, consumida só pelo `device_id` que a criou, expira em 15 min (5.2.4).
- `sessions` (6.2.5): segredo de 32 bytes, guardado como `sha256`; 7 dias, renovada a cada uso com mais de 1 dia (3.1.7); extrator `Volunteer` com perfil, `global_roles` e (vazio por enquanto) `memberships`.
- Perfil `volunteers` (7): nome, telefone, `show_phone`, idioma. Banido → `403 banned`.

**Papéis globais (2.1)**
- CLI `taba role grant|revoke <volunteer_id> validator|civil_defense`, com auditoria.
- Os valores `mediator` e `protection` existem no enum desde já; concessão pelo app só na fase 4.

**Auditoria**
- `audit.rs` e a coleção `audit_log` (7): **toda escrita a partir desta fase grava auditoria** (4.10), com `expires_at` de 1 ano. Anomalias e a política de TTL ficam para a fase 6.

**PWA**
- Telas `/login` e `/return` (3.1), formulário de perfil, `PATCH /me` (inclui idioma; ao logar, o idioma do perfil prevalece, 11.1).
- Várias sessões no aparelho e troca de operador por PIN (3.1.1): PIN de 6 dígitos criado no primeiro login online, guardado só como PBKDF2-SHA256 (200 000 iterações, salt próprio); troca funciona offline; cinco erros seguidos apagam a sessão daquele voluntário no aparelho. Tela `/switch-operator`.
- Offline: "Entrar" desabilitado com a mensagem certa.

**Infra**
- `env.j2` e `vault.yml.example` ganham `HMAC_KEY` (`vault_taba_hmac_key`), `DOMAIN` (de `taba_domain`), `UNICO_URL`, `SESSION_DAYS`.
- Backup da `HMAC_KEY` fora da VM (6.3.2, 10.5): perdê-la torna todos os IDs incomparáveis com tokens novos.
- TTL do Firestore para `sessions.expires_at` e `captures.expires_at` (em `firestore.indexes.json`/tarefa do `provision.yml`).
- No console da Unico, cadastrar a organização com o nome "Taba" e, se possível, o logotipo (5.1): a jornada mostra esse nome à pessoa.

## Fora de escopo
Convites (fase 2: usam a mesma jornada com `purpose = invite`); captura para check-in, revalidação e biometria (fase 3); modo degradado completo e `taba unico resume` (fase 3); concessão de `mediator`/`protection` pelo app (fase 4).

## Seções da spec
2.1, 3.0, 3.1, 3.1.1, 4.10 (só a gravação de auditoria), 5.1–5.6, 6.2.4, 6.2.5, 6.3.2, 7 (`volunteers`, `sessions`, `captures`, `audit_log`), 8.1, 10.1.1 (credenciamento: `fraud_prevention`), 10.5, 11.1.

## Critérios de aceite da seção 15
- **1** — `extract_token` devolve `None` para `value` ausente, `""` e `"  "`; nenhum desses cria voluntário.
- **6** — captura consumida uma vez e só pelo `device_id` que a criou.
- **7** — (manual) jornada aberta da PWA instalada no iPhone volta pelo `callbackUri` e gera sessão. O teste de viabilidade já foi feito (5.1); aqui ele é refeito com o login real em produção.
- **57** — troca de operador por PIN offline; cinco PINs errados apagam a sessão daquele voluntário no aparelho.

## O que a fase cria
| Tipo | Nomes |
|---|---|
| Coleções | `volunteers`, `sessions`, `captures`, `audit_log` |
| Rotas | `POST /v1/captures` (`purpose: login`), `POST /v1/sessions`, `DELETE /v1/sessions/current`, `GET /v1/me`, `PATCH /v1/me` |
| Erros | `profile_required`, `banned`, `capture_pending`, `capture_inconclusive`, `capture_failed`, `capture_canceled`, `unico_unexpected_state`, `unico_busy`, `unico_unavailable`, `unico_misconfigured`, `unico_limit`, `image_too_large` |
| CLI | `taba role grant|revoke` |
| Variáveis | `HUMAN_TOKEN_ACCESS_KEY`, `UNICO_URL`, `DOMAIN`, `HMAC_KEY`, `SESSION_DAYS` |
| PWA | `/login`, `/return`, `/switch-operator`, tela de perfil; `config.sessions`, `config.active_volunteer_id` |

## Dependências
Fase 0 (API, PWA, CI, deploy, IndexedDB).

## Testes
**Unitários (obrigatórios):** `extract_token` (ausente, vazio, espaços, válido); mapeamento de cada estado do processo e de cada erro HTTP da Unico (servidor falso); backoff do `429` e corte em 4 tentativas; semáforo; derivação HMAC (determinística, 43 caracteres, sem `/`); captura de uso único, de outro `device_id` e expirada; criação, renovação e expiração de sessão; banido; CLI de papéis; gravação de auditoria; PBKDF2 do PIN, troca de operador e bloqueio após cinco erros; idioma do perfil prevalece ao logar.

**Manuais com a Unico** (cada chamada é cobrada; anote no PR quantas foram feitas):
- Login no iPhone com o app instalado (1 processo por tentativa).
- Login num Android (1 processo).
- Segundo login da mesma pessoa em outro aparelho: mesmo `volunteer_id` (1 processo).
- Esperado: cerca de 3 a 5 processos.

## Riscos e pontos de atenção
- A jornada volta numa carga nova da página (5.1): todo estado necessário vem da URL (`capture_id`) e do IndexedDB.
- O nome da organização no console da Unico aparece para a pessoa; com outro nome, gera desconfiança.
- Login exige internet e Unico disponível; quem já tem sessão no aparelho continua trabalhando (base para o modo degradado da fase 3).
- O PIN protege só contra uso casual do aparelho; não substitui a sessão do servidor.
- Problemas novos na documentação do Human Token: registrar no arquivo interno de problemas da documentação da Unico.

## Checklist de pronto
- [ ] Cliente Unico com todos os estados e erros, testado com servidor falso.
- [ ] Login, perfil, sessão de 7 dias e banimento funcionando.
- [ ] Troca de operador por PIN offline.
- [ ] CLI de papéis com auditoria; toda escrita grava `audit_log`.
- [ ] `HMAC_KEY` no vault, com backup fora da VM; `env.j2` e `vault.yml.example` atualizados.
- [ ] Organização "Taba" no console da Unico.
- [ ] Critérios 1, 6, 7 e 57 verificados; 61 e 62 conferidos de novo.
- [ ] Testes manuais com a Unico feitos e contados no PR.
- [ ] QA aprovado, CI verde, PR mergeado pelo dono, deploy verificado em produção.

## Regras de processo (resumo; detalhe no README)
Senior staff engineer implementa; QA compara com a spec e com este doc item a item. Uma branch e um PR; o dono revisa; depois do merge, deploy em `taba.thingsbuilder.com`. Testes unitários em tudo; testes automáticos nunca chamam a Unico. Código, commits e PRs em inglês. Nada da instalação no repo. Problemas na documentação da Unico: registrar no arquivo interno de problemas da documentação da Unico. Pronto = QA aprovado + CI verde + PR mergeado + deploy verificado.
