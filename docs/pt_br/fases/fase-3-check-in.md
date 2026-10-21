# Fase 3 — Check-in (primeiro marco útil)

> Leia antes: [README das fases](README.md) e a [spec](../spec/spec.md). Este documento recorta a spec; se houver contradição, vale a spec e o dono decide.

## Objetivo
O abrigo cadastra as pessoas que chegam: com selfie (verificado), com foto do voluntário (assistido) ou sem biometria, organiza os núcleos familiares com cartão/pulseira, protege crianças separadas ou desacompanhadas, funciona sem internet e sem a Unico, mostra a lotação e registra a saída com destino. **É o primeiro marco útil:** a partir desta fase, um abrigo real pode usar o Taba.

**Entrega demonstrável em produção:** num abrigo de teste, fazer check-in de uma pessoa com selfie, de outra só com foto, de uma terceira que recusa a selfie; juntá-las num núcleo com cartão; registrar uma criança `unaccompanied` e ver o alerta com os telefones de referência; desligar a rede, cadastrar duas pessoas e ver tudo sincronizar ao religar; ver a lotação "N de capacidade"; registrar a saída de um adulto com destino.

## Escopo
**Consentimento (3.4, 10.1.3, 11.3)**
- Tela com finalidade, prazo, quem acessa, responsável pelo piloto (`PILOT_RESPONSIBLE`), aviso de abrigo não confirmado, as frases "Recusar a selfie não tira abrigo nem doação" e "Os dados não são compartilhados com polícia nem com imigração", versão curta para leitura em voz alta.
- Dois botões de mesmo peso: "Fazer a selfie" e "Prefiro não fazer" (design/README.md).
- Seletor próprio PT | EN | ES, independente do idioma do voluntário; `es.json` passa a ter as chaves dessa tela.
- Voluntário marca a base legal (`consent`, `vital_interest`, `child_protection`).
- Página `/privacy` com o texto completo.

**Três caminhos de cadastro (3.4, 4.1)**
- A, liveness: `purpose = checkin`; transação sobre `people/{person_id}`; cria `verified` ou sobe `assisted`; fecha `stay` em outro abrigo (`checkin_elsewhere`) e abre a nova.
- B, foto pela câmera do app (`getUserMedia` + `canvas`, sem galeria): `api+humantoken` → `assisted`; selfie oferecida depois, nunca exigida; lista "Pendentes de selfie".
- C, sem biometria: `no_biometrics`, `person_id` = UUID do cliente, `biometrics_declined` quando houve recusa; caminho de primeira classe.
- Biometria tardia `PUT /v1/people/{id}/biometrics` (4.12): migra o registro para o ID derivado do token, reaponta `households.member_ids`, `head_id`, `guardians` (e `deliveries`/`rescues`, que passam a existir nas fases 4 e 5 — a função precisa estar pronta para elas), junta `stays` se o token já existia, guarda `former_ids`, gera `household_conflict` se for o caso.
- Dados: `name`, `social_name`, `wristband`, `birth_year`, `needs` (lista fechada de quatro booleanos), `vulnerabilities` (3.4, 7).
- Miniatura de pessoa sem `name` nem `wristband` (4.6): 128×128 gerada no celular, cifrada no servidor com AES-256-GCM (`THUMBNAIL_KEY`), apagada na mesma escrita que preencher `name` ou `wristband`; acesso auditado por `GET /v1/people/{id}/thumbnail`.
- Visibilidade do histórico (4.11): admin de abrigo confirmado vê `stays` de outros abrigos; voluntário vê só `has_other_stays`; abrigo não confirmado vê só as próprias.

**Núcleo familiar e cartão (3.5.1)**
- `households` (membros, chefe, animais como contagem), uma pessoa em no máximo um núcleo; `cards` de 8 caracteres Crockford base32 gerados no celular, QR, um cartão ativo por núcleo, cancelamento com substituto; válidos na rede.
- Leitura de cartão e núcleo sem abrigo atual nem histórico.
- Leitura de QR com `BarcodeDetector` e `jsQR` de reserva.

**Criança (3.4.1, 3.8, 4.9)**
- `child_status` (`with_guardian`, `separated`, `unaccompanied`); responsáveis `verified` com vínculo `declared` ou `document_checked` (quem conferiu e quando).
- `separated`/`unaccompanied` → alerta `child_separated` para proteção, DC e admins do abrigo, mostrando `events.reference_contacts` (sem push).
- Mudança de `child_status` só por `protection` ou DC.
- Saída de criança: captura `purpose = revalidation` do responsável; voluntário/admin só com `with_guardian` + `document_checked`; demais casos e saída por exceção só por `protection` ou DC (`403 protection_role_required`); revalidação falhou → `409 revalidation_failed` e alerta `revalidation_failed` para a fila de proteção; exceção gera `exceptional_exit`.
- O Human Token funciona para crianças; a miniatura continua sendo o recurso para bebês e crianças sem identificação.
- O papel `protection` já é reconhecido pela autorização; a concessão pelo app chega na fase 4. Em produção, nesta fase, use DC (que tem os mesmos poderes aqui) ou um papel definido direto no teste.

**Saída e lotação (3.8, 4.2)**
- `POST /v1/shelters/{id}/exits` exige `exit_destination` (`own_home`, `relatives_or_friends`, `rental_aid`, `other_shelter`, `other`, `unknown`) e aceita `destination_municipality`.
- Tela do abrigo mostra "N de capacidade" (cartão de lotação do design), com vagas.

**Offline e fotos cifradas (6.1.7, 9)**
- Toda foto que espera na fila é cifrada com AES-GCM (WebCrypto) com `photo_key` não exportável no IndexedDB, e apagada após o `2xx` (`crypto.ts`).
- Fila: check-in por foto e sem biometria, núcleo, cartão, edição de pessoa e saída de adulto funcionam offline.
- `401`: itens de outro voluntário só seguem com a sessão dele ou se um admin os assumir ("Assumir pendências de <nome>", `taken_over_from` na auditoria) (9.2, M1).
- Conflitos (9.4): mesma pessoa em dois aparelhos; mesma pessoa em dois abrigos; pessoa em dois núcleos (`409 person_in_other_household`); dois cartões (`409 household_has_card`); edição por `client_edited_at`.

**Redução de imagem (6.1.4)**
- Foto para token: maior lado 1280 px; qualidade de 0,9 caindo 0,05 até ficar abaixo de 800 KB. Servidor recusa acima com `413 image_too_large`.

**Modo degradado completo (5.5, 5.7)**
- `429` com "monthly process limit" → `system/status` `degraded`/`limit`, alerta `unico_limit`, nenhuma chamada até `taba unico resume`.
- Três falhas seguidas → `degraded`/`unavailable` por 5 min.
- Rotas dependentes → `503 unico_degraded`; itens da fila esperam `GET /status` voltar a `ok`.
- Faixa no app: "Reconhecimento facial indisponível. O cadastro continua por nome e pulseira."; check-in segue pelo caminho C, com a foto cifrada na fila como biometria tardia.

## Fora de escopo
Resgate, busca, mediação, concessão de `mediator`/`protection` pelo app, encerramento de evento (fase 4); itens, entregas e duplicidade (fase 5); painel, exportação, anomalias e descarte (fase 6).

## Seções da spec
3.4, 3.4.1, 3.5.1, 3.8, 4.1, 4.2, 4.6, 4.9, 4.11, 4.12, 5.1, 5.3, 5.5, 5.7, 6.1.3, 6.1.4, 6.1.7, 6.2.4, 7 (`people`, `households`, `cards`, `system/status`, `alerts`), 8.5, 8.1 (`GET /status`), 9 (inteira), 10.1.1, 10.1.3, 10.3 (leitura de cartão e núcleo), 11.3.

## Critérios de aceite da seção 15
- **2** — `429` de limite mensal: sem repetição, `system/status` `degraded`/`limit`, alerta `unico_limit`; `429` comum: até 4 tentativas e `503 unico_busy`.
- **3** — em `limit`, nenhuma chamada até `taba unico resume`; rotas dependentes → `503 unico_degraded`.
- **5** — imagem > 800 KB → `413` sem chamar a Unico; o app produz fotos abaixo de 800 KB.
- **23** — mesmo token em dois check-ins → um documento, duas `stays`, uma aberta.
- **24** — check-in offline gera `assisted`; captura com liveness depois → mesmo documento, `verified`.
- **26** — biometria tardia cria `people/{hmac}`, reaponta núcleo (e entregas, quando existirem), apaga o antigo, guarda `former_ids`; se o `hmac` existia, junta as `stays`.
- **34** — pessoa em dois núcleos → `409 person_in_other_household`; segundo cartão → `409 household_has_card`.
- **35** — `GET /cards/{code}` e `GET /households/{id}` sem abrigo atual nem histórico.
- **36** — voluntário de abrigo confirmado vê só `has_other_stays`; admin vê as `stays`.
- **37** — saída de adulto sem `exit_destination` → `422` (a contagem por destino no painel é verificada na fase 6, critério 52).
- **38** — `needs` só aceita os quatro booleanos; texto livre → `422`.
- **39** — preencher `name` apaga `thumbnail_encrypted` na mesma escrita.
- **40** — miniatura por voluntário de outro abrigo → `403`; acesso permitido grava auditoria.
- **42** — criança `unaccompanied` gera `child_separated` com os telefones de `reference_contacts`.
- **43** — saída de criança `separated` por admin → `403 protection_role_required`; por `protection` com revalidação ok → `200`.
- **44** — `with_guardian` com vínculo `declared` por voluntário → `403`; com `document_checked` e revalidação ok → `200`.
- **45** — revalidação falhou → `409`; exceção por admin → `403`; por `protection` ou DC → `200` e `exceptional_exit`.
- **55** — reenviar o mesmo item da fila não gera segunda chamada à Unico.
- **56** — item com `401` não segue com outro voluntário; após "Assumir pendências" segue, com `taken_over_from`.
- **58** — `pt-PT` → pt-BR; `es` → inglês, exceto a tela de consentimento, em espanhol; escolha persiste e acompanha o perfil.

## O que a fase cria
| Tipo | Nomes |
|---|---|
| Coleções e documentos | `people`, `households`, `cards`; escrita em `system/status` |
| Rotas | `POST /v1/shelters/{id}/checkins`, `GET /v1/shelters/{id}/people`, `GET /v1/shelters/{id}/people/needs-selfie`, `GET/PATCH /v1/people/{person_id}`, `PUT /v1/people/{person_id}/biometrics`, `POST /v1/people/{person_id}/guardians`, `GET /v1/people/{person_id}/thumbnail`, `POST /v1/shelters/{id}/exits`, `POST /v1/shelters/{id}/households`, `GET/PATCH /v1/households/{id}`, `POST /v1/households/{id}/cards`, `GET /v1/cards/{code}`, `POST /v1/cards/{code}/cancel`; `POST /v1/captures` com `purpose: checkin|revalidation|biometrics` |
| CLI | `taba unico resume` |
| Alertas | `child_separated`, `revalidation_failed`, `exceptional_exit`, `household_conflict`, `unico_limit` |
| Erros | `no_face`, `invalid_image`, `unico_degraded`, `protection_role_required`, `revalidation_failed`, `guardian_not_verified`, `person_in_other_household`, `household_has_card`, `code_taken` |
| Variáveis | `THUMBNAIL_KEY`, `PILOT_RESPONSIBLE` |
| PWA | `/shelter/:id/checkin`, `/shelter/:id/people`, `/shelter/:id/households/:household_id`, `/shelter/:id/exit`, `/person/:id`, `/privacy`; `crypto.ts`, `camera.ts`, `image.ts`, `qr.ts`; faixa do modo degradado; "Assumir pendências" |
| Índices | `people (current_shelter_id, updated_at)`, `households (member_ids)`, `cards (household_id, status)` |

## Infra
- `env.j2` e `vault.yml.example`: `THUMBNAIL_KEY` (`vault_taba_thumbnail_key`) e `PILOT_RESPONSIBLE`.
- Índices novos no `firestore.indexes.json`.

## Dependências
Fase 2 (abrigos, eventos com `reference_contacts`, membros, alertas, idempotência) e fase 1 (cliente Unico, sessão, auditoria; o PIN está desligado, ver Riscos).

## Testes
**Unitários (obrigatórios):** níveis e transições (nunca desce); transação de check-in e fechamento de `stay`; biometria tardia (token novo, token existente, conflito de núcleo, reapontamentos); validação de `needs` e `vulnerabilities`; cifra e decifra da miniatura (e nonce único); apagamento da miniatura; visibilidade do histórico por papel e confirmação; regras de núcleo e cartão (unicidade, código Crockford, cancelamento); todas as regras de criança e saída (matriz 2.3); `exit_destination` obrigatório; redução de imagem abaixo de 800 KB; cifra das fotos da fila (o `Blob` gravado não é JPEG legível); "Assumir pendências"; conflitos da 9.4; modo degradado (`limit`, `unavailable`, `resume`, `503`); i18n da tela de consentimento em três idiomas.

**Manuais com a Unico** (anote no PR quantas chamadas foram feitas):
- Check-in com selfie (1).
- Check-in por foto da mesma pessoa em outro abrigo: mesmo `person_id` (1).
- Biometria tardia de uma pessoa `no_biometrics` com selfie (1).
- Saída de criança com revalidação do responsável, uma vez com o responsável certo e uma com outra pessoa (2).
- Esperado: cerca de 5 a 8 processos.

**Manuais sem a Unico:** cadastro offline completo no iPhone com o app instalado; leitura de cartão por QR no iPhone (`jsQR`) e no Android (`BarcodeDetector`); modo degradado provocado com o servidor falso num ambiente de teste.

## Riscos e pontos de atenção
- Fase grande: considere dividir o trabalho em commits por bloco (consentimento, caminhos, núcleo, criança, offline, degradado), mas um único PR.
- O Safari pode apagar o IndexedDB fora do modo standalone, e desde a fase 2 a instalação da PWA é **opcional** no iPhone (spec 3.0.2). Decisão do dono: **o risco do cadastro offline sem instalação é aceito nesta fase**; o item da fase 6 ainda revê isso.
- O PIN obrigatório do operador está desligado (`PIN_REQUIRED = false`, decisão do dono na fase 2; revisão na fase 6): a troca de operador só aceita quem já tinha PIN, e o resto do trabalho não depende dele.
- A cifra das fotos protege contra leitura dos arquivos do navegador fora do app, não contra quem usa o app desbloqueado.
- Não dá para provocar o limite mensal real da Unico: teste o modo degradado com o servidor falso.
- A tela de consentimento é lida por quem acabou de perder a casa: siga o design/README.md (frases curtas, nunca culpar a pessoa).

## Checklist de pronto
- [ ] Consentimento em três idiomas, com as frases obrigatórias e botões de mesmo peso.
- [ ] Três caminhos de cadastro e biometria tardia.
- [ ] Miniatura cifrada, visibilidade do histórico, núcleo e cartão.
- [ ] Regras de criança, alertas e saídas com destino; lotação na tela.
- [ ] Fila offline com fotos cifradas, "Assumir pendências" e conflitos.
- [ ] Modo degradado completo e `taba unico resume`.
- [ ] `THUMBNAIL_KEY` e `PILOT_RESPONSIBLE` no vault/inventário; índices aplicados.
- [ ] Critérios 2, 3, 5, 23, 24, 26, 34–40, 42–45, 55, 56 e 58 verificados; 61 e 62 conferidos de novo.
- [ ] Testes manuais com a Unico feitos e contados no PR.
- [ ] QA aprovado, CI verde, PR mergeado pelo dono, deploy verificado em produção.

## Regras de processo (resumo; detalhe no README)
Senior staff engineer implementa; QA compara com a spec e com este doc item a item. Uma branch e um PR; o dono revisa; depois do merge, deploy em `taba.thingsbuilder.com`. Testes unitários em tudo; testes automáticos nunca chamam a Unico. Código, commits e PRs em inglês. Nada da instalação no repo. Problemas na documentação da Unico: registrar no arquivo interno de problemas da documentação da Unico. Pronto = QA aprovado + CI verde + PR mergeado + deploy verificado.
