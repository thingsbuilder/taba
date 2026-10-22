# RIPD — Relatório de Impacto à Proteção de Dados Pessoais do piloto do Taba

> **Leitura técnica, não é parecer jurídico.** Antes de usar com dados reais, este documento deve ser validado por um advogado ou pelo DPO (encarregado) do controlador. Ele descreve o que o sistema faz, conferido no código e na [spec](spec/spec.md) (seção 10). Onde os dois divergem, vale o código e a divergência está em [Pontos a confirmar](#14-pontos-a-confirmar). O que não está no código nem na spec aparece como "A confirmar".

- Base: LGPD, art. 38 (RIPD), e a [base legal](<base legal LGPD.md>) do projeto.
- Versão: piloto (fases 0 a 6 da spec).
- Estado da implementação: neste documento, o painel, a exportação do abrigo, o descarte em `purge_at` e as anomalias (fase 6) são descritos como a spec 3.10, 4.10, 8.8 e 10.1.2 os definem. Na base de código usada para escrever este relatório essas quatro partes ainda eram esqueleto (rotas respondem `501`, o descarte não apaga nada). Releia as seções 7, 9 e 10 contra o código final da fase 6.

## 1. Identificação

| Papel | Quem |
|---|---|
| Controlador | **Não definido no piloto** (spec 1.1, 10.1.4, Q10, A9). Será uma organização real (por exemplo, a Defesa Civil ou a prefeitura) quando houver uso operacional. |
| Responsável pelo piloto | Pessoa ou organização informada na variável `PILOT_RESPONSIBLE` da instalação (nome e contato). As telas de coleta mostram esse valor (spec 10.1.3). |
| Operador técnico | Quem hospeda e mantém a instância (VM, Firestore, chaves). No piloto, a mesma parte que define `PILOT_RESPONSIBLE`: **A confirmar**. |
| Suboperador | Unico (Human Token), que gera o token a partir da foto. Ver seção 9. |
| Encarregado (DPO) | **A confirmar.** Nada na spec nem no código define um canal do encarregado. |

Quando o controlador existir, ele assume as decisões sobre prazos (por exemplo, a retenção das entregas, Q50), contrato com a Unico e mecanismo de transferência internacional (art. 33).

## 2. O que o tratamento faz

O Taba é um aplicativo web (PWA) para abrigos e resgate em desastres. Voluntários credenciados:

- registram pessoas em abrigos (check-in) e resgatadas, por selfie com prova de vida, por foto da câmera do app ou só por nome e pulseira;
- agrupam pessoas em núcleos familiares e emitem cartão para entregas;
- registram entregas de doação com checagem de duplicidade;
- registram buscas de desaparecidos por foto, com mediação quando há correspondência;
- acompanham crianças desacompanhadas e a saída de crianças;
- reportam números agregados à Defesa Civil (painel e CSV).

O reconhecimento facial é feito pela Unico (Human Token). O Taba guarda só um identificador derivado do token, nunca a foto nem o token puro.

## 3. Dados tratados e onde ficam

### 3.1 Por categoria

| Categoria | Campos | Titular | Sensível? | Onde |
|---|---|---|---|---|
| Identificador derivado do rosto | `person_id` = HMAC-SHA256 do token (43 caracteres) | pessoa atendida, voluntário | Sim (biometria: derivado de dado biométrico) | Firestore: ID de `people`, `volunteers`; `searches.person_ids`; `matches.person_id`; `rescues.person_id`; `deliveries.person_id` |
| Identificação civil | `name`, `social_name`, `wristband`, `birth_year` (todos opcionais) | pessoa atendida | Não (`birth_year` ajuda a identificar criança) | Firestore `people`; cache no celular (IndexedDB `people`) |
| Saúde | `needs`: medicação contínua, restrição alimentar, mobilidade reduzida, acompanhamento de saúde (só sim/não; a tela orienta a não escrever diagnóstico) | pessoa atendida | Sim | Firestore `people.needs`; cache no celular |
| Vulnerabilidade | gestante, lactante, deficiência por tipo, povo indígena (texto), idioma de migrante (texto) | pessoa atendida | Sim (origem étnica, saúde) | Firestore `people.vulnerabilities`; cache no celular |
| Criança | `is_child`, `child_status`, responsáveis (`guardians`, vínculo declarado ou com documento conferido, quem conferiu e quando) | criança e responsável | Dado de criança (art. 14) | Firestore `people` |
| Passagem por abrigos | `stays` (abrigo, evento, entrada, saída, motivo, destino da saída, município de destino), `event_ids`, `current_shelter_id` | pessoa atendida | Não, mas revela paradeiro | Firestore `people` |
| Resgate | lat/lon e precisão do ponto de desembarque, hora, nome do ponto, observação, voluntário e aparelho | pessoa resgatada | Revela local e momento | Firestore `rescues` |
| Miniatura | JPEG 128x128, só para pessoa sem nome e sem pulseira | pessoa atendida | Sim (imagem do rosto) | Firestore `people.thumbnail_encrypted`, cifrada. Antes do envio, no celular, dentro de `blobs` cifrado |
| Núcleo familiar e cartão | membros, chefe declarado, animais (só espécie e contagem), código do cartão (8 caracteres) | pessoa atendida | Não | Firestore `households`, `cards`; cache no celular |
| Entrega | item, quantidade, data, abrigo, verificação, quem aprovou, `person_id`, `household_id`, `card_code` | pessoa atendida | Não | Firestore `deliveries` |
| Busca de desaparecido | nome, apelido e observações do desaparecido; nome, telefone, parentesco e idioma de quem busca; `person_ids` derivados das fotos | desaparecido; familiar que busca | Observações podem conter dado sensível | Firestore `searches`. Antes do envio, no celular, o corpo da busca fica na fila (IndexedDB `queue`) |
| Voluntário | nome, telefone (opcional), "mostrar meu telefone" (sim/não), idioma, papéis globais, banimento, `last_seen_at`, `person_id` derivado do rosto | voluntário | Sim (biometria derivada) | Firestore `volunteers`, `memberships`, `sessions` |
| Sessão e aparelho | segredo de sessão (só o hash fica no servidor), `device_id` | voluntário | Não | Firestore `sessions/{sha256(segredo)}`; segredo no IndexedDB do celular |
| Auditoria | momento, voluntário, aparelho, abrigo, ação, alvo, base legal | voluntário | Não (só IDs, sem nome nem foto) | Firestore `audit_log` |
| Alertas | tipo, alvo, detalhe | variado | Não | Firestore `alerts` |
| Idempotência | resposta da última escrita de cada item da fila (7 dias) | variado | Pode conter dados da resposta | Firestore `idempotency` |

### 3.2 O que nunca é guardado

- **Foto de pessoa no servidor.** Ela só existe na memória do processo durante a chamada à Unico. Nenhum documento do Firestore tem foto, salvo a miniatura cifrada.
- **Token puro da Unico.** O servidor grava só o HMAC (`HMAC_KEY`). Nenhuma resposta da API devolve o token (critério 59).
- **Foto na fila do celular depois do envio.** O `Blob` cifrado é apagado quando o item recebe `2xx`.
- **Localização do aparelho em auditoria.** O `AuditEntry` do código não tem `ip`, `lat` nem `lon` (a spec 7 prevê esses campos: ver seção 14).

### 3.3 No celular (IndexedDB `taba`)

| Store | Conteúdo | Cifrado? |
|---|---|---|
| `blobs` | fotos e miniatura aguardando envio | Sim, AES-GCM 256 com `photo_key` não exportável |
| `queue` | requisição pendente: caminho, corpo (nome, dados do cadastro, contato da família na busca), voluntário, estado | **Não** (só as fotos são cifradas); apagado quando o item recebe `2xx` |
| `people`, `households`, `cards`, `shelters` | cópia do abrigo atual para uso offline | Não |
| `config` | `device_id`, idioma, sessões dos voluntários do aparelho, `photo_key`, hash do PIN (se existir), log de diagnóstico | Segredo de sessão em claro; `photo_key` não exportável |
| `config.diagnostics_log` | últimos 200 eventos do aparelho, sem nome, telefone, foto, código, token nem corpo (filtro `sanitize`); IDs cortados em 6 caracteres | Não, mas sem dado pessoal |

A cifra das fotos protege contra a leitura dos arquivos do navegador fora do app, não contra quem usa o app desbloqueado (spec 6.1.7).

## 4. Finalidades

| # | Finalidade | Operações |
|---|---|---|
| F1 | Localizar de novo a pessoa, evitar cadastro duplicado, reunir família | check-in, resgate, núcleo familiar |
| F2 | Proteger a vida e a integridade de pessoa que não pode consentir (ferida, incapacitada, desaparecida) | check-in por foto da câmera, resgate, miniatura, busca, mediação |
| F3 | Proteger a criança e localizar seus responsáveis | check-in de criança, alerta `child_separated`, saída de criança |
| F4 | Registrar o que cada pessoa recebe e evitar entrega duplicada ou fraudulenta | entrega, revalidação, revisão |
| F5 | Garantir que só voluntários identificáveis operem e permitir banir quem abusa | credenciamento, sessão, convite, banimento |
| F6 | Dar à Defesa Civil a contagem de desabrigados e a visão da operação | painel, CSV diário |
| F7 | Prestar contas e investigar uso indevido | auditoria, anomalias informativas |

Os dados não podem ser usados para outra finalidade (polícia, imigração, marketing, cruzamento com outras bases; LGPD art. 6º, I). O texto de consentimento diz isso à pessoa: "Os dados não são compartilhados com polícia nem com imigração".

## 5. Bases legais por operação

O servidor grava a base em `audit_log.legal_basis` (spec 10.1.1). Os valores do enum são `consent`, `vital_interest`, `fraud_prevention`, `public_policy`, `child_protection` e `family_consent`.

| Operação | `legal_basis` | Dispositivo | Observação |
|---|---|---|---|
| Check-in de adulto consciente, com ou sem selfie | `consent` | art. 11, I | Selfie é oferecida, não exigida. Recusar não tira abrigo nem doação |
| Check-in de pessoa ferida ou incapacitada; resgate; miniatura | `vital_interest` | art. 11, II, "e" | O voluntário marca a base no check-in |
| Check-in e saída de criança; alertas de criança | `child_protection` | art. 14, § 3º | Contato com responsáveis e proteção da criança |
| Entrega de doação e revalidação | `fraud_prevention` | art. 11, II, "g" | Liveness só em item de alto valor e por sorteio (5%) |
| Busca de desaparecido e mediação | `vital_interest` | art. 11, II, "e" | O titular, o desaparecido, não pode consentir |
| Contato da família que busca | `family_consent` | art. 7º, I | Ver ponto a confirmar sobre o registro dessa base |
| Credenciamento de voluntário | `fraud_prevention` | art. 11, II, "g" | Spec 10.1.1, Q35 |
| Painel e operação da Defesa Civil | `public_policy` | art. 11, II, "b" | Só dados agregados no painel |

O que o código grava hoje: `consent`/`vital_interest`/`child_protection` no check-in (conforme escolha do voluntário) e na pessoa criada; `vital_interest` em resgate, miniatura, busca, matches e decisão; `child_protection` em saída de criança e responsáveis; `fraud_prevention` em sessão, convite e entrega. `family_consent` e `public_policy` existem no enum, mas nenhuma rota de código os grava ainda.

Limites das bases (da [base legal](<base legal LGPD.md>)): `vital_interest` não autoriza outra finalidade, guarda indefinida, coleta além do necessário, nem uso quando o consentimento é viável (adulto consciente no check-in usa `consent`).

## 6. Titulares e dados sensíveis

| Titular | Dados sensíveis | Observação |
|---|---|---|
| Pessoa em abrigo (adulto) | biometria (token derivado), saúde (`needs`), origem étnica e deficiência (`vulnerabilities`) | Pode recusar a selfie e seguir sem biometria |
| Criança | os mesmos, mais `child_status` e responsáveis | Human Token funciona para criança (Q49). Bebê ou criança sem identificação: miniatura |
| Pessoa resgatada | biometria (por foto, nível `assisted`), local e hora do resgate | Pode estar inconsciente |
| Desaparecido | biometria derivada de foto enviada pela família | Não sabe da busca; não pode consentir |
| Familiar que busca | nome, telefone, parentesco | Contato só chega a quem atende depois que a pessoa encontrada aceitar |
| Voluntário | biometria derivada (login), nome, telefone | Credenciamento por rosto |

## 7. Quem acessa (resumo do código)

| Dado | Quem lê |
|---|---|
| Pessoas do abrigo (nome, necessidades, vulnerabilidades, criança) | membros ativos do abrigo (`GET /shelters/{id}/people`) |
| Histórico de outros abrigos | abrigo `confirmed`: admin vê as passagens; voluntário só vê "tem outras passagens: sim/não". Abrigo `unconfirmed`: só o próprio. `mediator`, `protection` e DC: só enquanto atendem match ou alerta sobre a pessoa (4.11) |
| Miniatura | membros do abrigo atual; quem atende match aberto sobre a pessoa; para resgatado sem abrigo, o voluntário que registrou o resgate e a DC. Cada leitura grava `thumbnail.view` com `vital_interest` |
| Cartão e núcleo | só nomes e nível dos membros, sem abrigo atual nem histórico |
| Busca (resposta) | idêntica com ou sem match: `202 search_registered` |
| Match | fila do abrigo (admins e `mediator`), central (`mediator` e DC) ou proteção (`protection` e DC, sempre para criança). O contato da família só aparece quando a pessoa aceita |
| Painel e CSV diário | `validator` (leitura) e DC; sem nome nem `person_id` |
| Exportação do abrigo | admin de abrigo `confirmed`: pessoas, núcleos e entregas do próprio abrigo, sem miniatura nem `person_id`, auditada |
| Auditoria e alertas | admin (alertas não informativos dos seus abrigos), `protection`, DC |
| Firestore e chaves | quem opera a instância (conta de serviço da VM; acesso ao console do projeto GCP) |

Não há pesquisa livre por nome na rede (Q22).

## 8. Fluxo da busca de desaparecidos e link da família

1. Operador de abrigo `confirmed`, DC, `mediator` ou `protection` registra a busca (ou gera o link da família). Limite: `SEARCHES_PER_DAY` (10) por voluntário por dia no fuso `TZ_PILOT`. A tela diz que a busca não substitui o registro na delegacia.
2. De 1 a 5 fotos (até 800 KB cada) vão ao servidor. Sem sinal, ficam cifradas na fila do celular e são apagadas após o `2xx`.
3. O servidor envia cada foto à Unico (flow `api+humantoken`), recebe o token, deriva o HMAC e descarta foto e token. Em `searches` ficam só os `person_ids`, os dados informados e o voluntário em nome de quem a busca está. Cada busca grava auditoria com quem buscou (`vital_interest`).
4. O servidor compara os `person_ids` com `people`. Cada check-in ou resgate posterior é comparado com as buscas ativas. Correspondência cria um `match` (ID determinístico).
5. A resposta ao operador é sempre a mesma, com ou sem match. Nem ele sabe se houve correspondência.
6. Quem atende o match (nunca o abrigo de quem buscou) vê nome, apelido, parentesco e observações informados pela família, o nome no cadastro, a miniatura (se houver) e o abrigo. Confere os dados com a pessoa antes de decidir. Decisões: `accept`, `decline` ou `not_the_person` (falso positivo).
7. Só em `accept` o atendente recebe nome, telefone, parentesco e idioma da família e um link `wa.me` com mensagem pronta. A tela orienta apagar o número depois. A resposta com o contato não é guardada na tabela de idempotência (o código a substitui por `match_already_decided`).
8. A localização da pessoa nunca é enviada automaticamente.
9. **Link da família** (opcional): o operador gera um link de uso único válido por 12 horas. A família abre `/f/<code>` (público, sem login) e envia dados e fotos da própria galeria. A busca fica em nome do operador, com o limite diário, a auditoria e a mediação dele. O operador não lê o conteúdo enviado. Limites: no máximo 5 tentativas com falha por link; 20 envios por hora por aparelho e IP, 100 por hora por IP (contadores na memória do processo). O link só é consumido quando ao menos uma foto gerou token.
10. Ao encerrar o evento, cada busca ativa sem match aceito gera o alerta `search_expiring` para quem a registrou, com a data do descarte e a orientação de procurar a delegacia e a Cruz Vermelha.
11. A busca fica ativa até o descarte do evento do abrigo de origem. Todos os dados dela, inclusive o contato da família, são apagados no descarte (seção 10).

## 9. Compartilhamento e suboperadores

| Destinatário | O que recebe | Base e limite |
|---|---|---|
| Polícia, imigração, outros órgãos | **Nada.** Não há integração nem exportação para eles (S2iD, CadÚnico, saúde e Cruz Vermelha estão fora do piloto, spec 1.3) | Frase obrigatória no consentimento |
| Unico (Human Token) | Foto da pessoa (check-in por foto, resgate, busca) ou a selfie feita na jornada da Unico (login, check-in com liveness, revalidação). Devolve um token. O servidor envia à Unico a foto em base64 e, na jornada, a `callbackUri` (com um identificador da captura). **Não envia** nome, telefone nem outro dado do cadastro | Operadora/suboperadora. A Unico prevê descartar a selfie depois de gerar o token. Os termos permitem foto que não veio do SDK (Q6). Contrato e DPA: **A confirmar** |
| Nominatim (OpenStreetMap) | Coordenadas do abrigo, enviadas pelo celular para obter o endereço (no máximo 1 requisição por segundo) | Terceiro público. Posição do abrigo, não dado do titular. Ver seção 14 |
| WhatsApp (Meta) | O telefone e a mensagem pré-pronta, quando o atendente toca no link `wa.me` depois do aceite | Ação do atendente no aparelho dele; fora do servidor |
| Google Cloud (Firestore, VM) | Todo o conteúdo do Firestore | Operador de infraestrutura. Ver seção 12 |
| Defesa Civil / validador | Agregados do painel e CSV, sem nome nem `person_id` | `public_policy` |

## 10. Retenção e descarte

| Dado | Prazo | Mecanismo |
|---|---|---|
| Miniatura | Na hora em que `name` ou `wristband` for preenchido (mesma escrita). Quando todos os eventos da pessoa estiverem encerrados (tarefa horária). Pessoa sem evento nem abrigo: `PURGE_AFTER_DAYS` após a última alteração | `drop_thumbnail_if_identified`; `jobs.rs` (`person.thumbnail_purge` na auditoria) |
| Pessoas, núcleos, cartões, resgates, buscas (com `person_ids` e contato da família), matches, alertas, memberships e abrigos do evento | `purge_at` = encerramento do evento + `PURGE_AFTER_DAYS` (30 dias). Pessoa só é apagada quando todos os seus eventos venceram | Tarefa horária de descarte (`purge.rs`) |
| Entregas | Pseudonimizadas no `purge_at`: somem `person_id` e `card_code`; `household_id` vira `household_pseudonym` aleatório por núcleo; ficam abrigo (nome), item, escopo, quantidade, data e status. Apagadas em `delivery_purge_at` = encerramento + `DELIVERY_RETENTION_DAYS` (90 dias no piloto; com controlador, o prazo é dele) | Mesma tarefa |
| Evento | Fica só com nome, datas e `stats`, `status = purged` | Mesma tarefa |
| Perfil de voluntário | 365 dias após `last_seen_at`; banidos ficam | Mesma tarefa |
| Auditoria | 1 ano (`expires_at`, política TTL do Firestore) | TTL |
| Sessões | 7 dias (`SESSION_DAYS`), renovadas no uso; banimento apaga | TTL |
| Capturas (15 min), convites, códigos de aval, link da família (12 h) | Vencimento do próprio documento | TTL e tarefa horária |
| Idempotência | 7 dias | TTL |
| Foto de pessoa | Nunca no servidor. No celular, até o `2xx` do item | Código do app |

**Quando o evento encerra:** por todos os abrigos fechados, ou por inatividade de `INACTIVITY_DAYS` (30). O evento encerrado não reabre.

**Chave de segurança do descarte.** O descarte apaga de forma irreversível. A variável `PURGE_ENABLED` vem **desligada** (`false`). Desligada, a tarefa horária só conta o que apagaria (modo seco) e registra as contagens no log e na auditoria. O dono da instalação a liga (`purge_enabled: true` no inventário) depois de testar com um evento de teste. **Enquanto estiver desligada, nenhum dado vence.** O RIPD só vale como "descarte em 30 dias" depois que a chave for ligada.

Lotes de 400 escritas; a tarefa para ao passar de 15 000 exclusões no dia (cota da camada gratuita) e retoma na hora seguinte. O descarte de um evento grande pode levar mais de um dia.

Sem backup do Firestore no piloto (Q32). Cópias de segurança manuais, se existirem, ficam fora desta regra: **A confirmar**.

## 11. Segurança

| Medida | Como é no código e na spec |
|---|---|
| Biometria só como derivado | `person_id` = HMAC-SHA256(`HMAC_KEY`, token). O HMAC impede cruzar bases em caso de vazamento. Perder a `HMAC_KEY` torna os IDs incomparáveis; ela tem backup fora da VM |
| Miniatura cifrada | AES-256-GCM, chave `THUMBNAIL_KEY` (32 bytes), nonce aleatório de 12 bytes por cifra, limite de 32 KB, só JPEG. Nunca vira token. Resposta com `Cache-Control: no-store` |
| Fotos na fila do celular | AES-GCM 256 (WebCrypto), `photo_key` não exportável no IndexedDB, nonce aleatório de 12 bytes por foto, apagadas após `2xx` |
| Sessões | segredo de 32 bytes aleatórios; no servidor só o `sha256`; 7 dias; banimento apaga as sessões |
| Papéis | Matriz da spec 2.3 conferida por handler: voluntário, admin de abrigo, `validator`, `civil_defense`, `mediator`, `protection`. `mediator` e `protection` só atribuídos por validador ou DC |
| Auditoria | Toda escrita e todo acesso a miniatura e a dados de match gravam `audit_log` (ID, ação, alvo, base legal, sem nome nem foto), transação junto com a escrita, TTL de 1 ano |
| Anomalias | Alertas informativos para a DC: volume de check-ins (30 em 10 min) e resgates (100 em 10 min) por voluntário, mesma pessoa a mais de 50 km em menos de 1 h, resgate fora da área. Não bloqueiam |
| Rotas públicas | `GET /status`, `POST /captures` (só login e convite), `POST /sessions`, `GET` e aceite de convite, link da família. Corpo máximo de 6 MB, imagem de até 800 KB. Só o link da família tem limite próprio (seção 8). Os demais não têm limite no piloto (spec 10.4, Q15); abuso consome créditos e o fim dos créditos liga o modo degradado |
| Idempotência | `Idempotency-Key` repetida devolve a resposta gravada, para voluntário e rota iguais; chave de outro voluntário ou rota é recusada (`idempotency_key_mismatch`). A resposta com contato de família nunca é gravada |
| Segredos | `HUMAN_TOKEN_ACCESS_KEY`, `HMAC_KEY`, `THUMBNAIL_KEY` só no vault da instalação e em `/etc/taba/env` na VM. Nunca no repositório nem no celular |
| Aparelho compartilhado | O aparelho guarda as sessões dos voluntários que entraram nele. A troca de operador pode exigir PIN local (PBKDF2-SHA256, 200 000 iterações). **No piloto o PIN está desligado** (`PIN_REQUIRED = false`) |
| Modo degradado | Sem a Unico, o cadastro continua por nome e pulseira. A foto, se houver, espera cifrada na fila |
| Diagnóstico no celular | Log local sem dado pessoal |

### 11.1 Proteção contra abrigo falso (spec 10.2)

- O abrigo nasce `unconfirmed`: vê só o que cadastrou, não busca, não media, não exporta. A tela de consentimento mostra "Abrigo não confirmado pela Defesa Civil".
- Criar abrigo exige perfil com liveness: o criador é identificável e banível.
- Só validador ou aval presencial confirmam. O aval presencial exige dois celulares a até 100 m com precisão de até 100 m e código de 10 minutos. Revogação sinaliza quem avalizou.
- Abrigo a menos de 100 m de outro: bloqueado ou marcado `flagged` para a DC.

### 11.2 Proteção contra agressor que busca a vítima (spec 10.3)

- Busca só por operador de abrigo confirmado, DC, `mediator` ou `protection`, com limite diário.
- Resposta da busca igual com ou sem match.
- Quem media é do abrigo onde a pessoa está, `mediator`, `protection` ou DC, nunca o abrigo de quem buscou.
- O mediador confere os dados com a pessoa; falso positivo vira `not_the_person`.
- O contato da família só aparece depois do aceite; localização nunca é enviada.
- Voluntário comum não vê por quais abrigos a pessoa passou; leitura de cartão não devolve abrigo nem histórico.
- A busca grava auditoria com quem buscou.
- **Risco residual:** agressor que é admin do mesmo abrigo da vítima (seção 13, R1).

## 12. Região e transferência internacional

- O piloto roda em `us-east1` (Carolina do Sul, EUA): VM e2-micro e Firestore. A camada gratuita da e2-micro só existe em `us-east1`, `us-central1` e `us-west1`.
- Isso é **transferência internacional de dados** (LGPD, art. 33) para o Google (infraestrutura) e, pela chamada de reconhecimento facial, para a Unico (a região de processamento da Unico: **A confirmar**).
- Mecanismo do art. 33 e controlador: definidos quando houver uso com uma organização real (spec 10.1.4, Q10, A9). No piloto: **A confirmar**.
- Cada controlador pode subir a própria instância em `southamerica-east1` (São Paulo). O custo é sair da camada gratuita. Não muda o código: só `gcp_region` e `gcp_zone` no inventário Ansible.
- Recomendação técnica: com dados reais de pessoas, usar `southamerica-east1` ou obter o mecanismo do art. 33 antes.

## 13. Riscos e medidas

Severidade (impacto no titular) e probabilidade são **avaliação técnica preliminar** do autor, para o advogado ou o DPO ajustarem. Escala: baixa, média, alta.

| # | Risco | Sev. | Prob. | Medidas | Risco residual |
|---|---|---|---|---|---|
| R1 | **Agressor admin do mesmo abrigo da vítima** vê a vítima no abrigo | Alta | Baixa | Busca só por operador confirmado e com limite; resposta igual com ou sem match; `mediator` pode atender a fila do abrigo; auditoria de cada acesso a miniatura e match; DC pode banir e remover. Spec 10.3 | **Mantido.** Admin do abrigo vê as pessoas do próprio abrigo por desenho. Registrado como risco residual |
| R2 | Agressor usa a busca para achar a vítima | Alta | Média | Seção 11.2: resposta idêntica, mediação, contato só após aceite, sem localização automática, cota diária, auditoria | Baixo |
| R3 | **Abrigo falso** capta dados de pessoas | Alta | Baixa | `unconfirmed` sem busca, mediação nem exportação; criador com liveness e banível; confirmação por validador ou aval presencial; alerta de proximidade | Baixo |
| R4 | **Operador mal-intencionado** (voluntário credenciado) consulta ou copia dados | Alta | Média | Papéis mínimos; sem pesquisa livre por nome; auditoria em escrita e em acesso a miniatura e match; anomalias informativas; banimento; identificação por rosto | Médio. As anomalias só informam, não bloqueiam; ninguém monitora em tempo real |
| R5 | **Vazamento** do Firestore ou das chaves | Alta | Baixa | Foto nunca guardada; token só como HMAC; miniatura cifrada; segredos fora do git; conta de serviço só com `roles/datastore.user`; descarte em 30 dias | Médio. Nome, necessidades de saúde, vulnerabilidades, telefone da família e `stays` ficam em claro no Firestore. Sem backup |
| R6 | **Falso positivo de match** expõe contato da família a quem não é a pessoa, ou liga pessoas erradas | Média | Média | Mediador confere os dados com a pessoa antes de aceitar; `not_the_person`; só o aceite libera o contato; indicador no painel | Baixo |
| R7 | **Coerção ao consentimento**: pessoa em abrigo sente que precisa dar a selfie para receber abrigo ou doação | Média | Média | Recusa tem caminho de primeira classe (`no_biometrics`); texto "Recusar a selfie não tira abrigo nem doação"; dois botões de mesmo peso; taxa de recusa por abrigo no painel; voluntário marca a base legal | Médio. Depende da conduta do voluntário |
| R8 | Criança tratada sem o consentimento dos pais | Média | Alta | Base `child_protection` (art. 14, § 3º); alerta imediato para `protection`; saída só com responsável conferido ou com `protection`/DC; alertas de exceção à DC | Baixo |
| R9 | **Dado de saúde e vulnerabilidade visível a todos os voluntários do abrigo** | Média | Média | Campos fechados (sim/não), sem diagnóstico; só membros do abrigo; exportação restrita a admin de abrigo confirmado | Médio |
| R10 | **Perda ou roubo do aparelho** com sessões e cache de pessoas | Média | Média | Fotos cifradas; sessão de 7 dias; 5 PINs errados apagam a sessão (quando o PIN existir); banimento apaga sessões no servidor | **Alto enquanto o PIN estiver desligado.** O cache de pessoas e a fila (nome, contato da família) ficam em claro no celular |
| R11 | **Perda dos dados do navegador** (Safari apaga IndexedDB após ~7 dias sem uso quando o app não está instalado) | Média | Média | Convite para instalar o app; `navigator.storage.persist()` | Médio. Cadastros offline ainda não enviados podem se perder |
| R12 | **Retenção além do prazo**: descarte desligado (`PURGE_ENABLED=false`) ou falho; entradas de idempotência fora do descarte | Média | Média | Modo seco mostra o que apagaria; lotes e retomada; TTL de 7 dias na idempotência | Médio até o dono ligar a chave e testar |
| R13 | **Uso secundário** (polícia, imigração) por pressão externa | Alta | Baixa | Nenhuma integração nem exportação para esses órgãos; descarte em 30 dias; texto de consentimento; CSV e painel sem identificação | Médio. Depende do controlador resistir a ordens; **A confirmar** |
| R14 | **Reidentificação por agregados pequenos** no CSV diário e no painel (um indígena num abrigo de 8 pessoas) | Média | Média | CSV sem nome nem `person_id` | Médio. Não há supressão de células pequenas: **A confirmar** |
| R15 | **Transferência internacional** sem mecanismo do art. 33 | Média | Alta (no piloto) | Documentada; instância própria em `southamerica-east1`; mecanismo a definir com o controlador | Alto até o controlador definir |
| R16 | Fornecedor (Unico) trata ou retém a foto além do necessário | Alta | Baixa | Servidor descarta a foto; Unico prevê descartar a selfie depois do token | Médio. Contrato e DPA: **A confirmar** |
| R17 | Abuso das rotas públicas (créditos da Unico, link da família) | Baixa | Média | Limites do link da família; corpo de 6 MB; modo degradado | Baixo. Sem limite nas demais rotas públicas |
| R18 | Perda da `HMAC_KEY` | Média | Baixa | Backup fora da VM | Baixo |

## 14. Pontos a confirmar

Cada item é uma decisão do dono, uma divergência entre spec e código, ou um fato que o repositório não responde.

**Decisões do dono (da fase 6)**

1. **Quem é o controlador.** Hoje só existe `PILOT_RESPONSIBLE`. Definir controlador, encarregado (DPO) e canal de contato dos titulares antes de dados reais. Sem isso, o texto de consentimento mostra apenas o responsável do piloto.
2. **PIN do operador desligado.** `PIN_REQUIRED = false` em `pwa/src/sessions.ts` (desligado na fase 2 a pedido do dono). Sem PIN, qualquer pessoa com o aparelho desbloqueado e com sessão no aparelho assina escritas e vê o cache. Decidir se volta como obrigatório e se o PIN sobrevive ao "Sair".
3. **Instalação opcional da PWA e perda de dados do Safari.** No iPhone, a instalação é opcional (spec 3.0). Fora da tela de início, o Safari apaga o IndexedDB após cerca de 7 dias sem uso, o que pode perder cadastros offline ainda não enviados e a `photo_key` (foto na fila fica ilegível, item vira `needs_action`). Decidir se a instalação volta a ser exigida para cadastro offline.
4. **Miniatura apagada ao preencher o nome.** A miniatura é apagada na mesma escrita que preenche `name` ou `wristband` (critério 39) e só existe no cadastro por foto da câmera do app, não no cadastro com selfie. Em teste de campo isso a fez sumir ao completar o cadastro. Decidir, com a revisão de LGPD, se ela fica sempre (para o voluntário reconhecer a pessoa), por quanto tempo e se vale gerá-la também no cadastro com selfie. Se ficar, a retenção e a base legal da miniatura mudam e este documento precisa ser revisto.
5. **Texto de consentimento em espanhol provisório.** A tradução ainda não foi validada por falante nativo nem por jurídico. O consentimento só vale se a pessoa entende o texto.

**Divergências entre spec e código**

6. **Fase 6 ainda não implementada na base deste relatório.** Painel (`dashboard.rs`), exportação do abrigo (`export.rs`), descarte (`purge.rs`) e anomalias (`audit::note_activity`) eram esqueleto. As seções 7 e 10 e as medidas de anomalia descrevem a spec. Reconferir depois do merge.
7. **`PURGE_ENABLED` padrão `false`.** Nenhum dado vence enquanto estiver desligada. Definir quando e quem liga, após teste com evento de teste.
8. **`audit_log` sem `ip`, `lat`, `lon`.** A spec 7 prevê esses campos. O código não os grava. Isto reduz a capacidade de investigar abuso, mas também o dado pessoal guardado. Decidir qual vale.
9. **`family_consent` e `public_policy` não são gravados.** A spec 10.1.1 liga o contato da família a `family_consent` e a operação da DC a `public_policy`. O código grava `vital_interest` na busca e nada para o painel. Alinhar spec e código.
10. **Miniatura "na hora" do encerramento.** A spec 10.1.2 diz "na hora"; o código a apaga na tarefa horária seguinte (até 1 hora depois).
11. **Texto do consentimento mais amplo que a regra.** O texto diz que "a rede de abrigos confirmados" vê o cadastro. Pelo código, só membros do abrigo atual veem o cadastro; o histórico completo é do admin de abrigo confirmado; a correspondência entre abrigos acontece por foto. Alinhar o texto à regra.

**Fatos que o repositório não responde (A confirmar)**

12. **Direitos dos titulares na prática.** Não há rota para o titular pedir acesso, eliminação ou revogação do consentimento. Correção: voluntário do abrigo edita o cadastro (`PATCH /people/{id}`). Eliminação sob demanda e revogação não têm rota nem procedimento; só o descarte automático. Definir o procedimento (quem atende, em quanto tempo, como apagar uma pessoa e o que fazer com os `person_ids` em `searches` e `matches`).
13. **Idempotência fora do descarte.** A tabela `idempotency` guarda por 7 dias as respostas `2xx` das escritas assinadas (por exemplo, a resposta do check-in, com `person_id` e histórico) e o `person_id` derivado de cada foto de busca (`photo~<chave>`). O descarte de 30 dias não a toca. Como o TTL é de 7 dias, o impacto é limitado, mas ele existe.
14. **Fila e cache no celular em claro.** O corpo dos itens da fila (nome, necessidades, vulnerabilidades, nome e telefone de quem busca) e os caches `people`, `households`, `cards` não são cifrados. Confirmar o que "Sair" apaga do aparelho e se há limpeza do cache ao trocar de abrigo.
15. **Unico.** Contrato, DPA, região de processamento, retenção real da selfie e do token, e canal de incidente. No resgate e na busca, a Unico recebe a foto de quem não está em condição de consentir ou nem sabe da busca.
16. **Logs e backup.** Se o proxy (Caddy) ou a VM gravam IP e rotas, por quanto tempo, e se alguém faz cópia manual do Firestore. A spec não prevê backup (Q32) e não descreve a retenção de logs.
17. **Nominatim.** Coordenadas do abrigo são enviadas ao serviço público do OpenStreetMap. Confirmar se isso é aceitável ou se o controlador prefere um serviço próprio.
18. **Incidente de segurança.** Sem procedimento de comunicação à ANPD e aos titulares (LGPD, art. 48).
19. **Células pequenas no painel e no CSV.** Decidir se haverá supressão abaixo de um limite (R14).
20. **Limites das rotas públicas.** `POST /captures`, `POST /sessions` e convites não têm limite de taxa (Q15 adiada). Decidir se o piloto aceita.
21. **Resistência a pedido de autoridade.** A recusa de repasse a polícia e imigração é uma regra de produto, não técnica. O controlador precisa assumi-la por escrito.

## 15. Aprovação

| Etapa | Quem | Data |
|---|---|---|
| Revisão jurídica | A confirmar | A confirmar |
| Revisão do encarregado (DPO) do controlador | A confirmar | A confirmar |
| Decisões da seção 14, itens 1 a 5 | Dono do projeto | A confirmar |
