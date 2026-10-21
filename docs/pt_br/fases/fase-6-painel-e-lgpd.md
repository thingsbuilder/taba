# Fase 6 — Painel e LGPD (pronto para o piloto)

> Leia antes: [README das fases](README.md) e a [spec](../spec/spec.md). Este documento recorta a spec; se houver contradição, vale a spec e o dono decide.

## Objetivo
Fechar o piloto: painel no formato do que a Defesa Civil reporta, CSV diário e exportação do abrigo, descarte diferenciado, anomalias informativas, auditoria completa e o RIPD. Ao fim desta fase, o Taba está **pronto para o piloto**.

**Entrega demonstrável em produção:** a DC abre `/dashboard`, vê o evento com capacidade, lotação e vagas por abrigo, recortes, saídas por destino e taxa de recusa, e baixa o CSV diário; um admin de abrigo confirmado exporta o CSV do abrigo; num evento de teste encerrado com `purge_at` no passado, a tarefa horária apaga pessoas, núcleos e buscas e pseudonimiza as entregas.

## Escopo
**Painel (3.10)**
- `GET /v1/dashboard/events` e `GET /v1/dashboard/events/{id}` para validador (leitura) e DC.
- Aviso fixo: "Conta desabrigados (pessoas em abrigo). Desalojados (em casa de parentes ou amigos) não estão aqui."
- Por evento e abrigo: confirmação, `flagged`, posição, capacidade, lotação e vagas; pessoas únicas com a proporção de `verified`, `assisted` e `no_biometrics`; faixas etárias (0–11, 12–17, 18–59, 60+); gestantes, lactantes, deficiência por tipo, crianças `separated`/`unaccompanied`, indígenas, migrantes; núcleos e animais; movimentações (24 h e 7 dias) e saídas por `exit_destination`; resgates; alertas abertos; matches nas filas central e de proteção; indicadores do piloto (taxa de recusa da selfie, `not_the_person`, justificativas de "outro local", processos Unico por pessoa atendida); abrigos aguardando aval.
- Contagens por agregação `count()`.
- `GET /v1/dashboard/events/{id}/daily.csv`: uma linha por abrigo, sem nome nem `person_id`.
- Tela `/dashboard` na PWA.
- **Abrigos a confirmar (lacuna da fase 2):** validador e DC precisam achar abrigos `unconfirmed` para confirmar, e hoje só chegam a um abrigo se souberem o ID (a tela inicial lista só os abrigos de que a pessoa é membro; `GET /v1/shelters` não existe). Lista na `/dashboard` (e atalho na tela inicial de quem tem `validator` ou `civil_defense`) com os abrigos aguardando aval ou confirmação, cada um levando a `/shelter/:id` com os botões de confirmar e revogar. Pode usar o campo "abrigos aguardando aval" do painel.

**Exportação do abrigo (8.8, Q22)**
- `GET /v1/shelters/{id}/export.csv` só para admin de abrigo `confirmed`: pessoas, núcleos e entregas do próprio abrigo, sem miniatura nem `person_id`, auditado. Abrigo `unconfirmed` → `403`.

**Descarte diferenciado (10.1.2, 6.2.6 item 3)**
- No `purge_at` (`closed_at + PURGE_AFTER_DAYS`, 30): apagar pessoas cujos eventos estão todos vencidos (IDs derivados e `no_biometrics`), núcleos, cartões, resgates, buscas, matches, alertas, memberships e abrigos do evento; pseudonimizar entregas (`person_id` e `card_code` apagados, `household_id` → `household_pseudonym` aleatório por núcleo, `pseudonymized = true`); evento fica com `name`, datas e `stats`, `status = purged`.
- No `delivery_purge_at` (`closed_at + DELIVERY_RETENTION_DAYS`, 90): apagar as entregas pseudonimizadas.
- Lotes de 400 escritas; parar ao passar de 15 000 exclusões no dia e retomar na hora seguinte.
- Perfis de voluntário apagados 365 dias após `last_seen_at`, exceto banidos.

**Auditoria e anomalias (4.10, 7)**
- Política de TTL de 1 ano em `audit_log.expires_at`.
- Revisão de que **toda** escrita e todo acesso a miniatura ou a dados de match gravam auditoria com a base legal da 10.1.1.
- Anomalias só informativas (`informational = true`, no painel, fora da caixa de "Avisos" dos admins): `anomaly_volume` (`ANOMALY_CHECKINS_PER_10MIN` = 30, `ANOMALY_RESCUES_PER_10MIN` = 100), `anomaly_distance` (> 50 km em < 1 h), `out_of_area`.

**Prontidão do piloto**
- `docs/pt_br/RIPD.md` com o conteúdo mínimo da 10.1.4 (dados, finalidades, bases, riscos, medidas, descarte, região e transferência internacional, instância própria em `southamerica-east1`). Documento em português.
- Rever o PIN do operador (3.1.1): foi desligado na fase 2 a pedido do dono (`PIN_REQUIRED = false` em `pwa/src/sessions.ts`); decidir se volta como obrigatório e se o PIN passa a sobreviver ao "Sair".
- Conferência final: nenhuma rota das fases posteriores (seção 16) existe; nenhum token puro nem foto de pessoa em resposta ou documento.

## Fora de escopo
Tudo da seção 16 (depois do piloto). Integração com S2iD, CadÚnico, saúde e Cruz Vermelha (1.3).

## Seções da spec
3.10, 4.10, 4.11, 6.2.2, 6.2.6, 6.3.4, 6.3.5, 7 (`events.stats`, `deliveries` pseudonimizadas, `audit_log`), 8.8, 10 (inteira), 13 (Q9, Q21, Q22, Q39, Q50), 15, 16 (só para conferir que nada dela foi construído).

## Critérios de aceite da seção 15
- **13** — não existem rotas de fusão de abrigos, aval remoto, fotos do local nem "Estou Seguro".
- **16** — abrigo `unconfirmed`: `POST /searches` → `403`; exportação → `403`; histórico do check-in sem `stays` de outros abrigos.
- **41** — 50 resgates em 10 min não geram `anomaly_volume`; 50 check-ins geram, com `informational = true` e fora da caixa de "Avisos" do admin.
- **52** — o painel conta `assisted` e `no_biometrics`, mostra capacidade, lotação e vagas, recortes, saídas por destino, taxa de recusa e o aviso sobre desalojados.
- **53** — `daily.csv` com uma linha por abrigo, sem nome nem `person_id`.
- **59** — nenhuma resposta contém o token puro; nenhum documento contém o token puro nem foto de pessoa (só miniatura cifrada).
- **60** — após o descarte, entregas sem `person_id` nem `card_code`; pessoas, núcleos e buscas do evento não existem mais.

## O que a fase cria
| Tipo | Nomes |
|---|---|
| Rotas | `GET /v1/dashboard/events`, `GET /v1/dashboard/events/{id}`, `GET /v1/dashboard/events/{id}/daily.csv`, `GET /v1/shelters/{id}/export.csv` |
| Campos | `events.stats`, `events.delivery_purge_at`, `deliveries.household_pseudonym`, `deliveries.pseudonymized` |
| Alertas | `anomaly_volume`, `anomaly_distance`, `out_of_area` (informativos) |
| Variáveis | `PURGE_AFTER_DAYS` (uso no descarte), `DELIVERY_RETENTION_DAYS` (90), `ANOMALY_CHECKINS_PER_10MIN`, `ANOMALY_RESCUES_PER_10MIN` |
| Módulos | `dashboard.rs`; descarte em `jobs.rs`; anomalias em `audit.rs` |
| PWA | `/dashboard` |
| Índices e TTL | `people (event_ids, level)`, `audit_log (shelter_id, at)`, `audit_log (volunteer_id, at)`; TTL `audit_log.expires_at` |
| Documentos | `docs/pt_br/RIPD.md` |

## Dependências
Todas as fases anteriores (o painel conta o que elas criam; o descarte apaga o que elas gravam).

## Testes
**Unitários (obrigatórios):** cada contagem e recorte do painel (com dados de fixture conhecidos); faixa etária por `birth_year`; CSV diário e exportação sem nome nem `person_id` e sem miniatura; permissão da exportação; descarte (pessoa com um evento vencido e outro ativo não é apagada; pseudonimização; entregas apagadas no `delivery_purge_at`; lotes e corte diário; retomada); perfis de voluntário antigos; cada regra de anomalia e o flag `informational`; varredura de que nenhum documento de teste contém token puro ou foto.

**Manuais com a Unico:** nenhum (0 chamadas).

**Manuais sem a Unico:** abrir o painel e baixar o CSV em produção; rodar o descarte num evento de teste com datas antecipadas.

## Riscos e pontos de atenção
- **Descarte é irreversível.** Teste com o emulador e depois num evento de teste em produção, nunca num evento com dados reais.
- **Cotas do Firestore** (6.3.5): o descarte de um evento grande leva mais de um dia; isso é esperado.
- **Contagens** com `count()` custam leituras; evite recalcular o painel a cada segundo (por exemplo, cache curto no servidor).
- O RIPD é leitura técnica, não parecer jurídico; antes de dados reais, valide com um advogado ou com o DPO do controlador.

## Checklist de pronto
- [ ] Painel completo e CSV diário.
- [ ] Validador e DC encontram os abrigos a confirmar sem saber o ID.
- [ ] Exportação do abrigo com as restrições.
- [ ] Descarte diferenciado, pseudonimização e perfis antigos.
- [ ] Anomalias informativas e auditoria revisada, com TTL de 1 ano.
- [ ] `docs/pt_br/RIPD.md` escrito.
- [ ] Rever o PIN do operador (3.1.1): foi desligado na fase 2 a pedido do dono (`PIN_REQUIRED = false`); decidir se volta como obrigatório e se o PIN passa a sobreviver ao "Sair".
- [ ] Conferência de que nada da seção 16 foi construído.
- [ ] Critérios 13, 16, 41, 52, 53, 59 e 60 verificados; 61 e 62 conferidos de novo.
- [ ] Todos os 62 critérios continuam verdes no CI.
- [ ] QA aprovado, CI verde, PR mergeado pelo dono, deploy verificado em produção.

## Regras de processo (resumo; detalhe no README)
Senior staff engineer implementa; QA compara com a spec e com este doc item a item. Uma branch e um PR; o dono revisa; depois do merge, deploy em `taba.thingsbuilder.com`. Testes unitários em tudo; testes automáticos nunca chamam a Unico. Código, commits e PRs em inglês. Nada da instalação no repo. Problemas na documentação da Unico: registrar no arquivo interno de problemas da documentação da Unico. Pronto = QA aprovado + CI verde + PR mergeado + deploy verificado.
