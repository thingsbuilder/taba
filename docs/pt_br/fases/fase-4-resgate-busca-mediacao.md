# Fase 4 — Resgate, busca e mediação

> Leia antes: [README das fases](README.md) e a [spec](../spec/spec.md). Este documento recorta a spec; se houver contradição, vale a spec e o dono decide.

## Objetivo
Registrar resgatados no ponto de desembarque, buscar desaparecidos por foto sem nunca revelar a localização a quem buscou, e mediar o reencontro pelas filas certas (abrigo, central, proteção). Inclui os papéis `mediator` e `protection` concedidos pelo app e o encerramento automático de evento, com aviso antes de apagar buscas.

**Entrega demonstrável em produção:** no ponto de desembarque, registrar um resgatado sem nome (miniatura) e ver os abrigos próximos com vagas; registrar uma busca com fotos dessa pessoa (inclusive offline); depois do check-in dela num abrigo confirmado, o admin vê o match em "Avisos", confere os dados, aceita e recebe o link de WhatsApp da família; uma busca de criança vai para a fila de proteção.

## Escopo
**Resgate (3.6)**
- Foto só pela câmera do app; GPS do ponto, `landing_point`, `destination_shelter_id`, observação.
- `POST /v1/rescues` pela fila: `api+humantoken` → `person_id`; cria `assisted` ou acrescenta o resgate ao histórico; sem rosto ou em modo degradado, cria `no_biometrics` e deixa a foto cifrada na fila como biometria tardia (fase 3). Evento: o do abrigo `open` mais próximo a ≤ `EVENT_RADIUS_KM`.
- Miniatura cifrada quando não há nome nem pulseira (4.6); quem vê a de resgatado sem abrigo: o voluntário do ponto de desembarque que registrou e a DC (Q33).
- `GET /v1/shelters/nearby` com capacidade e lotação, mostrado na tela de resgate.
- Matches com buscas ativas também no resgate.
- A biometria tardia (4.12) passa a reapontar `rescues.person_id`.
- A biometria tardia (4.12) também dispara a comparação com as buscas ativas, como o check-in e o resgate.

**Busca (3.7)**
- Quem registra: voluntário de abrigo `confirmed`, DC, `mediator` ou `protection`; abrigo `unconfirmed` → `403`.
- Aviso na tela: "Esta busca não substitui o registro de desaparecimento na delegacia."
- 1 a 5 fotos (galeria permitida), nome, apelido, observações, contato e idioma da família; `SEARCHES_PER_DAY` por voluntário (`429 search_limit`).
- Offline pela fila, com fotos cifradas no aparelho e apagadas após o `2xx` (fase 3, `crypto.ts`). No servidor, só as fotos de busca com match são retidas (abaixo).
- Um token por foto; guarda só os `person_id` derivados; um match em qualquer foto conta.
- **Retenção das fotos da família (decisão do dono, 2026-10-22, opção B):** todas as fotos (até 5) de toda busca ativa com foto utilizável ficam cifradas (`THUMBNAIL_KEY`) em `search_photos/{search_id}.{n}`, gravadas depois da resposta da busca (tempo igual com ou sem match); só JPEG e PNG. O mediador as vê em `/matches/:id` (`GET /v1/matches/{id}/search-photos/{n}`, auditado), também quando o match é criado depois da busca. Apagadas quando a pessoa é encontrada (match aceito e nenhum aberto), no encerramento do evento, no descarte e em 30 dias (`PURGE_AFTER_DAYS`); `decline` e `not_the_person` não apagam.
- **Resposta sempre igual**, com ou sem match: `202 {message: "search_registered", usable_photos}`.
- Busca ativa até o descarte do evento do abrigo de origem; cada check-in e resgate posterior é comparado.

**Busca pelo link da família (opcional; decisão do dono na fase 4, spec 3.7 item 11)**
- O voluntário que pode registrar buscas (3.7 item 1) escolhe "Enviar link para a família" em vez de preencher a busca; os dois caminhos convivem. `POST /v1/search-links` cria `search_links/{code}` (128 bits, base64url, uso único, **validade de 12 horas**, `created_by`, `shelter_id`, `event_id`, `status`) e devolve `{code, url, expires_at}`; o app abre o compartilhar do celular. Conta no `SEARCHES_PER_DAY` do voluntário na criação (`429 search_limit`). `DELETE /v1/search-links/{code}` cancela.
- Página pública `/f/<code>` (sem login, sem instalação, PT | EN | ES): aviso "Esta busca não substitui o registro de desaparecimento na delegacia.", consentimento (o que é guardado, por quanto tempo, quem acessa), dados do desaparecido e da família, 1 a 5 fotos escolhidas da galeria do celular da família. `GET /v1/search-links/{code}` (público) diz só se o link vale, sem revelar quem o gerou nem o abrigo. `POST /v1/search-links/{code}/submit` (público) usa o mesmo pipeline do `POST /v1/searches` (um token por foto, guarda só os `person_id` derivados, descarta as fotos, tenta o match na hora) e responde sempre igual: `202 {message: "search_registered", usable_photos}`. Não há fila offline: sem sinal, a família tenta de novo.
- O link só é consumido quando o processamento termina com sucesso; em modo degradado responde `503 unico_degraded` e o link continua valendo. Reenviar não paga de novo as fotos já processadas (idempotência). Link expirado, usado ou cancelado: `410 search_link_invalid`.
- A busca fica registrada em nome do voluntário que gerou o link (`created_by`, `via = family_link`): limite, auditoria e mediação ficam presos a ele. **O voluntário não lê o conteúdo enviado:** só vê em "Avisos" que a família enviou e `usable_photos`.
- Contenção de abuso nas rotas públicas: limite de tentativas por link e por aparelho, corpo máximo de 6 MB, imagem ≤ 800 KB, nada de dado pessoal em log; a spec 10.4 passa a listar as duas rotas.

**Mediação (4.8)**
- Match idempotente `matches/{sha256(search_id|person_id)}`.
- Filas: criança → `protection` (só `protection` e DC); abrigo `confirmed` → `shelter` (admins do abrigo e qualquer `mediator`); abrigo `unconfirmed` ou sem abrigo → `central` (`mediator` e DC). `mediator` vale para a rede toda.
- Quem decide primeiro fecha (transação que exige `status = open`; `409 match_already_decided`).
- Tela de conferência com os dados da família, sem o contato; decisão `accept`, `decline` ou `not_the_person`.
- `accept` devolve `family` e `whatsapp_link` no idioma da família, com a orientação "Depois de combinar o contato, apague este número do seu celular."
- `not_the_person` encerra sem contar como recusa e não gera novo match para o mesmo `person_id`.
- Auditoria com `legal_basis = vital_interest`; histórico visível a `mediator`/`protection`/DC só enquanto atendem (4.11).

**Papéis globais pelo app (2.1)**
- `POST /v1/volunteers/{id}/roles` e `DELETE /v1/volunteers/{id}/roles/{role}` para `mediator` e `protection`, só por validador ou DC, com `role_grants`.

**Encerramento de evento (4.3 itens 4–5, 6.2.6 itens 1, 2, 4)**
- Tarefa `tokio` horária no `serve`: encerra eventos com todos os abrigos `closed` ou sem atividade há `INACTIVITY_DAYS`; grava `purge_at`; apaga miniaturas das pessoas cujos eventos estão todos encerrados; gera `search_expiring` para quem registrou buscas ativas sem match aceito, com a data do descarte e a orientação de encaminhar à delegacia e à Cruz Vermelha; expira capturas, convites e códigos de aval vencidos.
- Abrigo de evento encerrado não aceita escrita (`409 event_closed`).
- O descarte em si (apagar dados no `purge_at`) é da fase 6.

**PWA**
- Telas `/rescue`, `/search`; matches na `/inbox`, com a tela de conferência; gestão de papéis para validador e DC.

## Fora de escopo
Doações (fase 5); descarte, painel, anomalias (fase 6); "Estou Seguro" e mediação `search_origin` (seção 16).

## Seções da spec
2.1, 2.3, 3.6, 3.7, 4.3, 4.6, 4.8, 4.11, 4.12, 5.3, 6.2.4, 6.2.6, 7 (`rescues`, `searches`, `matches`, `volunteers.role_grants`), 8.2 (`nearby`), 8.4 (papéis), 8.7, 10.1.1, 10.3, 11.1 (mensagem de WhatsApp).

## Critérios de aceite da seção 15
- **11** — evento sem escrita há 30 dias vira `closed` com `purge_at`; miniaturas apagadas; buscas ativas sem match geram `search_expiring`.
- **22** — só validador ou DC concedem `mediator`; voluntário comum → `403`.
- **46** — match de criança vai para `protection` e não aparece para admins do abrigo.
- **47** — `POST /searches` com corpo idêntico com e sem match; 11ª busca do dia → `429 search_limit`.
- **48** — busca offline: foto cifrada no IndexedDB e apagada após o `2xx`.
- **49** — match em abrigo confirmado aparece para admins e `mediator`; contato só com `accept`; segunda decisão → `409 match_already_decided`.
- **50** — `not_the_person` encerra, mantém a busca e não recria match para o mesmo `person_id`.
- **51** — check-in posterior de pessoa procurada cria match; repetir não cria outro.

## O que a fase cria
| Tipo | Nomes |
|---|---|
| Coleções | `rescues`, `searches`, `matches`, `search_links` (TTL em `expires_at`) |
| Rotas | `POST /v1/rescues`, `PATCH /v1/rescues/{id}`, `GET /v1/shelters/nearby`, `POST /v1/searches`, `GET /v1/matches`, `GET /v1/matches/{id}`, `POST /v1/matches/{id}/decision`, `POST /v1/volunteers/{volunteer_id}/roles`, `DELETE /v1/volunteers/{volunteer_id}/roles/{role}`; link da família: `POST /v1/search-links`, `DELETE /v1/search-links/{code}`, `GET /v1/search-links/{code}` e `POST /v1/search-links/{code}/submit` (as duas últimas públicas) |
| Alertas | `search_expiring`, `family_search_received` (informativo, só para quem gerou o link: "a família enviou" e `usable_photos`) |
| Erros | `search_limit`, `match_already_decided`, `event_closed`, `search_link_invalid` |
| Variáveis | `SEARCHES_PER_DAY`, `INACTIVITY_DAYS`, `PURGE_AFTER_DAYS` (só para calcular `purge_at`) |
| Módulos | `rescues.rs`, `searches.rs`, `jobs.rs` (encerramento) |
| PWA | `/rescue`, `/search` (com "Enviar link para a família"), página pública `/f/:code`, conferência de match na `/inbox`, gestão de papéis |
| Índices | `searches (person_ids, status)`, `searches (event_id, status)`, `matches (mediator_kind, mediator_shelter_id, status, created_at)`; TTL `search_links.expires_at` |

## Dependências
Fase 3 (pessoas, níveis, miniatura, biometria tardia, fila com fotos cifradas, modo degradado) e fase 2 (eventos, abrigos, alertas).

## Testes
**Unitários (obrigatórios):** resgate (criação `assisted`, histórico, evento pelo GPS, `no_biometrics` em modo degradado); `nearby` (raio, vagas); busca (permissões, limite diário, resposta idêntica byte a byte com e sem match, `usable_photos`); escolha da fila para cada situação (criança, confirmado, não confirmado, sem abrigo); "quem decide primeiro fecha" com decisões concorrentes; `accept` libera o contato só nesse momento; `not_the_person`; idempotência do match; concessão de papéis por papel; tarefa de encerramento (todos fechados, inatividade, `purge_at`, miniaturas, `search_expiring`); `409 event_closed`; mensagem de WhatsApp nos dois idiomas. Link da família: validade de 12 h, uso único (envios concorrentes: exatamente um sucesso), `503 unico_degraded` sem consumir o link, resposta idêntica com e sem match, o voluntário não recebe o conteúdo, limites por link e por aparelho, `search_limit` na criação.

**Manuais com a Unico** (cada foto de busca é uma chamada; anote no PR):
- Resgate de uma pessoa (1).
- Busca da mesma pessoa com 3 fotos antigas (3).
- Check-in dela com selfie para disparar o match (1).
- Esperado: cerca de 5 a 8 processos.

**Manuais sem a Unico:** mediação completa entre dois aparelhos (admin e `mediator`, quem decide primeiro); busca registrada offline e sincronizada.

## Riscos e pontos de atenção
- **Agressor que busca a vítima (10.3):** a resposta da busca não pode vazar nada, nem no tempo de resposta de forma óbvia (não devolva cedo quando não há match).
- **Falso positivo:** a conferência com a pessoa é obrigatória antes de aceitar; nunca pule essa tela.
- **Custo:** cada foto de busca é um processo cobrado; o limite diário existe por isso.
- O contato da família vai para o WhatsApp pessoal de quem media: a orientação para apagar é obrigatória.
- Filas paradas não servem: a `/inbox` precisa destacar matches abertos.

## Checklist de pronto
- [ ] Resgate no ponto de desembarque com miniatura, `nearby` e matches.
- [ ] Busca online e offline, com resposta idêntica e limite diário.
- [ ] Busca pelo link da família (opcional) com as duas rotas públicas protegidas.
- [ ] Três filas de mediação, conferência, `accept`/`decline`/`not_the_person`.
- [ ] Concessão de `mediator` e `protection` por validador ou DC.
- [ ] Tarefa horária de encerramento com `search_expiring`; `409 event_closed`.
- [ ] Índices aplicados.
- [ ] Critérios 11, 22 e 46–51 verificados; 61 e 62 conferidos de novo.
- [ ] Testes manuais com a Unico feitos e contados no PR.
- [ ] QA aprovado, CI verde, PR mergeado pelo dono, deploy verificado em produção.

## Regras de processo (resumo; detalhe no README)
Senior staff engineer implementa; QA compara com a spec e com este doc item a item. Uma branch e um PR; o dono revisa; depois do merge, deploy em `taba.thingsbuilder.com`. Testes unitários em tudo; testes automáticos nunca chamam a Unico. Código, commits e PRs em inglês. Nada da instalação no repo. Problemas na documentação da Unico: registrar no arquivo interno de problemas da documentação da Unico. Pronto = QA aprovado + CI verde + PR mergeado + deploy verificado.
