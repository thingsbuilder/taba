# Especificação técnica — Taba (Human Token para abrigos e resgate em desastres)

> Versão 2 da spec, 2026-10-21. Fontes: as decisões do dono (itens a–j, registradas na seção 13), `docs/pt_br/base legal LGPD.md`, `infra/README.md`, a revisão de um especialista em gestão de desastres e os testes com a API real (estes três últimos são documentos internos, fora do repositório) e a documentação pública da Unico Human Token.
>
> Convenções deste documento:
> - **Decisão do dono (a–j):** requisito. Citado como "[dec. a]" etc. Achados da revisão aceitos pelo dono são citados como "[dec. h, A1]".
> - **PROPOSTA (a confirmar):** solução minha para algo que as decisões não cobrem. Toda proposta aparece de novo na seção 12.
> - Sem marcação: escolha técnica de implementação, com a justificativa no registro de decisões (seção 13).
> - Leitura técnica da LGPD, não é parecer jurídico.
> - O texto é em português; **todo identificador é em inglês** (seção 0).

## Sumário

0. Convenções de nomes e idioma da implementação
1. Visão, objetivos e fora de escopo
2. Atores, papéis e matriz de permissões
3. Funcionalidades e fluxos
4. Regras de negócio
5. Integração com a Unico
6. Arquitetura e implantação
7. Modelo de dados (Firestore)
8. API REST
9. Sincronização offline e conflitos
10. Segurança e privacidade
11. Idiomas da interface (i18n)
12. Questões abertas e propostas a confirmar
13. Registro de decisões
14. Estrutura de arquivos do repositório
15. Critérios de aceite

---

## 0. Convenções de nomes e idioma da implementação [dec. j]

- **Tudo da implementação em inglês:** código, identificadores, rotas da API, payloads, coleções e campos do Firestore, valores de estado, papéis, variáveis de ambiente, crates, módulos, diretórios, mensagens de log, **mensagens de commit e descrições de pull request**.
- A interface do usuário é traduzida (seção 11). A documentação do projeto segue em português.
- Um nome por conceito. Tabela de referência:

| Conceito (pt) | Identificador (en) |
|---|---|
| Taba (projeto, binário, serviço) | `taba` |
| evento | `event` / coleção `events` |
| abrigo | `shelter` / `shelters` |
| voluntário (perfil) | `volunteer` / `volunteers` |
| vínculo de membro | `membership` / `memberships` |
| papel no abrigo: admin, voluntário | `admin`, `volunteer` |
| papéis globais: validador, Defesa Civil | `validator`, `civil_defense` |
| convite | `invite` / `invites` |
| aval (presencial, remoto) | `endorsement` (`in_person`, `remote`) |
| código de aval presencial | `endorsement_codes` |
| sessão | `session` / `sessions` |
| captura (jornada Unico) | `capture` / `captures` |
| pessoa atendida | `person` / `people` |
| nível: verificado, assistido, sem biometria | `verified`, `assisted`, `no_biometrics` |
| passagem por abrigo | `stay` / `stays` |
| responsável por criança | `guardian` / `guardian_ids` |
| pulseira (número improvisado) | `wristband` |
| cartão/pulseira com QR do núcleo familiar | `card` / `cards` |
| doação (entrega) | `delivery` / `deliveries` |
| item de doação | `item` |
| resgate | `rescue` / `rescues` |
| busca de desaparecido | `search` / `searches` |
| match | `match` / `matches` |
| fusão de abrigos | `merge` / `merges` |
| alerta | `alert` / `alerts` |
| auditoria | `audit_log` |
| miniatura | `thumbnail` |
| "Estou Seguro" | `safe_check` |
| não confirmado / confirmado | `unconfirmed` / `confirmed` |
| aberto / encerrado / fundido (abrigo) | `open` / `closed` / `merged` |
| ativo / encerrado / fundido / descartado (evento) | `active` / `closed` / `merged` / `purged` |
| descarte | `purge` |
| sinalizado | `flagged` |
| em revisão | `in_review` |
| painel | `dashboard` |
| idempotência | `idempotency` |

---

## 1. Visão, objetivos e fora de escopo

### 1.1 Visão
Sistema open source para abrigos e equipes de resgate em desastres (referência: enchentes do RS, 2024). Reconhece pessoas pelo rosto, sem documento, usando o Human Token da Unico: a mesma pessoa sempre recebe o mesmo token. O token não diz quem a pessoa é, só que é a mesma de antes. O sistema **não valida identidade** e não deve ser apresentado assim.

### 1.2 Objetivos da v1
1. Cadastro único de pessoas atendidas entre abrigos, com histórico que acompanha a pessoa.
2. Entrega de doações por cartão do núcleo familiar, com sinalização (sem bloqueio) de retirada repetida [dec. h, A1].
3. Registro de resgatados no ponto de desembarque ou triagem, ligado depois ao abrigo [dec. h, A11].
4. Credenciamento de voluntários com liveness.
5. Busca de desaparecidos por foto, mediada, sem nunca devolver a localização a quem buscou.
6. "Estou Seguro": autorregistro pelo celular; quem precisa de resgate é orientado a ligar 193/199 [dec. h, C3].
7. Revalidação na saída de criança e nas entregas de alto valor ou sorteadas [dec. h, A1].
8. Funcionar sem internet no abrigo, exceto o que depende do Human Token com liveness.
9. Custo de hospedagem dentro da camada gratuita do GCP, menos o IPv4 externo e o tráfego de saída de um evento real [dec. g, i].

### 1.3 Fora de escopo da v1
- App nativo [dec. a]; integração com S2iD e Cemaden [dec. c].
- Espanhol no resto da interface (só consentimento e "Estou Seguro" têm espanhol) [dec. h, M3].
- Alerta "preciso de resgate" gravado no sistema [dec. h, C3]. Volta quando um centro de operações se comprometer a monitorar a fila.
- Registro de resgate dentro do barco [dec. h, A11].
- Prontuário de saúde, triagem de benefício emergencial, cadastro de danos, horas e certificado de voluntário, selfie no início de cada turno (ideias 4–6 do histórico).
- Notificação push. **PROPOSTA (a confirmar):** avisos aparecem na tela "Avisos" quando o app é aberto (Q19).
- Busca de desaparecido feita pela própria família, sem operador. **PROPOSTA (a confirmar)** (Q8).
- Backoffice web: os papéis globais são concedidos por linha de comando na VM (2.1).

---

## 2. Atores, papéis e matriz de permissões

### 2.1 Atores

| Ator | Como existe no sistema | Autenticação |
|---|---|---|
| **Pessoa atendida** | Documento em `people`. Não usa o app (exceto no "Estou Seguro"). | Nenhuma; é reconhecida pelo rosto ou pelo cartão. |
| **Família** | Dados de contato dentro de uma `search`. Não usa o app. | Nenhuma. |
| **Voluntário** | Documento em `volunteers` + `memberships` com `role = volunteer`. Qualquer pessoa que fez Human Token com liveness tem perfil [dec. b]. Inclui o operador do ponto de desembarque [dec. h, A11]. | Sessão emitida após jornada com liveness. |
| **Admin de abrigo** | `memberships` com `role = admin`. O criador do abrigo é o admin inicial [dec. a]. | Igual ao voluntário. |
| **Validador** | `volunteers.global_roles` contém `validator` (Cruz Vermelha, prefeituras, Defesa Civil). Dado uma única vez, não por evento [dec. d]. | Igual ao voluntário. |
| **Defesa Civil** | `volunteers.global_roles` contém `civil_defense`. | Igual ao voluntário. |
| **Operador da instância** | Quem administra a VM. Não é papel no app. | SSH na VM. |

**PROPOSTA (a confirmar) — papéis globais (Q23):** `civil_defense` inclui todos os poderes de `validator` e mais: banir da rede, remover admin, ver o painel, mediar matches de abrigos não confirmados e de pessoas fora de abrigo. `validator` confirma e revoga abrigos e vê o painel (só leitura). Os dois são concedidos e retirados pelo operador da instância com `taba role grant|revoke <volunteer_id> <role>`, que grava auditoria.

### 2.2 Níveis de confiança do abrigo [dec. d]

| `confirmation` / `confirmation_type` | Como chega | `can_endorse` |
|---|---|---|
| `unconfirmed` | Criação; revogação | `false` |
| `confirmed` / `validator` | Validador confirma | `true` |
| `confirmed` / `in_person` | Admin de abrigo que pode avalizar, celulares a ≤ 100 m | `true` |
| `confirmed` / `remote` | Dois admins de dois abrigos que podem avalizar, mesmo evento | `false`, até receber aval presencial ou de validador |

### 2.3 Matriz de permissões

Legenda: V = voluntário do abrigo; A = admin do abrigo; Val = validador; DC = Defesa Civil; NC = abrigo `unconfirmed`; C = abrigo `confirmed`. "Próprio" = do abrigo em que o ator é membro ativo.

| Operação | V (NC) | V (C) | A (NC) | A (C) | Val | DC |
|---|---|---|---|---|---|---|
| Criar abrigo (qualquer perfil com liveness) | sim | sim | sim | sim | sim | sim |
| Check-in, emissão de cartão, entrega, saída no próprio abrigo | sim | sim | sim | sim | — | — |
| Ver os próprios cadastros | sim | sim | sim | sim | — | — |
| Ver histórico de outros abrigos de uma pessoa presente no próprio abrigo | não | sim | não | sim | — | — |
| Registrar resgate (ponto de desembarque) | sim | sim | sim | sim | — | sim |
| Registrar busca por foto na rede | não | sim | não | sim | — | sim |
| Mediar match (pessoa no próprio abrigo) | não | não | não | sim | — | sim (fila DC) |
| Ver miniatura de pessoa sem identificação no próprio abrigo | sim | sim | sim | sim | — | só ao mediar |
| Revisar entrega sinalizada | não | não | sim | sim | — | — |
| Configurar itens (período, alto valor) | não | não | sim | sim | — | — |
| Convidar voluntário (QR ou link) | não | não | sim | sim | — | — |
| Convidar admin (só QR presencial) | não | não | sim | sim | — | — |
| Promover voluntário a admin | não | não | sim | sim | — | — |
| Remover voluntário do abrigo | não | não | sim | sim | — | sim |
| Remover admin | não | não | não | não | — | sim |
| Banir da rede | não | não | não | não | — | sim |
| Pedir fusão com outro abrigo (só conflito offline) | não | não | sim | sim | — | — |
| Aceitar fusão no próprio abrigo | não | não | não | sim | — | — |
| Exportar dados do próprio abrigo | não | não | não | sim | — | — |
| Aval presencial / remoto | não | não | não | sim, se `can_endorse` | — | — |
| Confirmar ou revogar abrigo | não | não | não | não | sim | sim |
| Painel da rede | não | não | não | não | leitura | sim |
| Renomear evento | não | não | sim | sim | — | sim |
| Encerrar o próprio abrigo | não | não | sim | sim | — | sim |

Regras de membros [dec. b]: admin não remove admin; o abrigo sempre tem pelo menos um admin ativo; o último admin só sai depois de promover outro. Um admin pode sair se não for o último.

---

## 3. Funcionalidades e fluxos

"Captura com liveness" = o app pede uma captura ao servidor, abre o link da jornada da Unico (flow `humantoken`), a pessoa faz a selfie, o app volta e o servidor obtém o token (5.2). Exige internet [dec. i, Q4].

"Foto pela câmera do app" = `getUserMedia` + quadro capturado em `canvas`, sem acesso à galeria (histórico, "Uso sem liveness", item 4). Gera nível `assisted`.

### 3.0 Pré-requisitos do aparelho
1. Primeiro acesso: o app gera `device_id` (UUID v4) e guarda no IndexedDB.
2. No iPhone, a primeira tela pede para instalar na tela de início antes de qualquer cadastro [dec. a]. O app detecta `navigator.standalone !== true` em iOS e bloqueia a criação de abrigo e os cadastros offline até a instalação.
3. Ao instalar ou entrar, o app chama `navigator.storage.persist()` [dec. a]. Se a resposta for `false`, mostra um aviso permanente "os dados deste celular podem ser apagados pelo navegador; sincronize com frequência".

### 3.1 Credenciamento e login de voluntário (funcionalidade 4)
1. Tela "Entrar". O app chama `POST /v1/captures {purpose: "login"}` e recebe `{capture_id, link}`.
2. O app grava `capture_id` no IndexedDB e navega para `link`.
3. A jornada redireciona para `https://<domain>/return?capture=<id>`. O app chama `POST /v1/sessions {capture_id}`.
4. Perfil existe: recebe a sessão. Não existe: `422 profile_required`; o app mostra o formulário (nome, telefone opcional, "mostrar meu telefone a quem tentar criar abrigo perto do meu" [dec. a], idioma) e reenvia `POST /v1/sessions {capture_id, profile}`.
5. Perfil banido: `403 banned`.
6. Offline: "Entrar" fica desabilitado com a mensagem "precisa de internet".

### 3.2 Criação de abrigo [dec. a, b, c]
1. Pré-condição: sessão válida (qualquer voluntário).
2. O app lê `navigator.geolocation.getCurrentPosition({enableHighAccuracy: true, timeout: 30000, maximumAge: 0})` e grava `lat`, `lon`, `accuracy`.
3. `accuracy > 100 m`: o app pede para ir a um lugar aberto e tentar de novo.
4. Online: o app chama o Nominatim (reverse) direto do celular e preenche o endereço. Offline: guarda as coordenadas com `address.pending = true` e busca depois (6.1.6).
5. O operador digita o nome ou apelido. Fotos do local (sem pessoas) são opcionais; **PROPOSTA (a confirmar):** até 2, reduzidas a ≤ 200 KB (Q29).
6. Online: `GET /v1/events/nearby?lat&lon`. Se houver eventos ativos na região ou no estado e nenhum a ≤ R km, o app pergunta "Este abrigo recebe pessoas de algum destes eventos?" [dec. c]. **PROPOSTA (a confirmar):** "região ou estado" = eventos com algum abrigo na mesma UF ou a ≤ 300 km (Q31).
7. O app gera `shelter_id` (UUID v4) e envia `POST /v1/shelters` (direto ou pela fila).
8. Servidor, sob o lock global de criação (6.2.4):
   1. Valida `accuracy ≤ 100`.
   2. Procura abrigos `open` a ≤ `SHELTER_RADIUS_M`. Se achar e o pedido não trouxer `other_site_reason`: `409 shelter_nearby` com `{shelter_id, name, distance_m, contact: {first_name, phone?}}`. `phone` só se o criador marcou `show_phone`.
   3. Com `other_site_reason`: cria com `flagged = true` e alerta `shelter_flagged` para a DC.
   4. Atribui evento (4.3).
   5. Cria o abrigo `unconfirmed`, a `membership` do criador como `admin` e grava auditoria.
9. Na resposta `409`, o app mostra "Já existe o abrigo X; procure <primeiro nome>" e duas saídas: "É outro local? Explique." (reenvia com `other_site_reason`) e, se o abrigo foi criado offline e já tem cadastros, "Fundir meus cadastros no abrigo X" (3.2.1).

#### 3.2.1 Conflito offline e fusão [dec. a, i]
1. Dois abrigos criados sem sinal a < 100 m: o segundo a sincronizar recebe o `409`.
2. Os itens da fila daquele abrigo ficam `held` (9.3).
3. O admin escolhe "Explique" (vira abrigo `flagged`, os itens seguem) ou "Fundir".
4. "Fundir" cria o abrigo de origem no servidor como `unconfirmed` (para receber os itens retidos) e um pedido em `merges`. Um admin do abrigo de destino aceita ou recusa [dec. i]. Na aceitação, o servidor move pessoas, cartões, entregas e resgates para o destino (o token resolve as repetidas, 9.4), transforma os membros da origem em `volunteer` do destino e marca a origem `status = merged`. Antes de aceitar, o destino não vê nada da origem, e a origem nunca vê nada do destino além do nome e do primeiro nome do criador.

### 3.3 Convite de voluntários [dec. b]
1. Admin toca "Convidar voluntário", escolhe nível (`volunteer` padrão, ou `admin`) e forma:
   - **QR presencial** (`kind = qr`): uso único, expira em 30 min, nível `admin` ou `volunteer`.
   - **Link remoto** (`kind = link`): uso único, expira em 24 h, **só `volunteer`**.
2. `POST /v1/shelters/{id}/invites` → `{code, url, expires_at}`. QR e link carregam só `https://<domain>/invite/<code>`. Exige internet.
3. O convidado abre a URL; o app mostra `GET /v1/invites/{code}` (nome do abrigo, nível, se é confirmado).
4. O convidado sempre faz captura com liveness (`purpose: "invite"`). Quem já tem perfil só faz essa captura, que é a revalidação [dec. b]; quem não tem preenche o perfil.
5. `POST /v1/invites/{code}/accept {capture_id, profile?}` → cria a `membership` e devolve a sessão.
6. Admin vê os convites ativos e cancela (`DELETE /v1/invites/{code}`). Quem convidou vê quem entrou pelo link e pode remover.
7. Recusas: convite expirado, usado ou cancelado (`410 invite_invalid`); perfil banido (`403 banned`); já é membro ativo (`409 already_member`); removido daquele abrigo antes (`403 removed_from_shelter`; **PROPOSTA (a confirmar)**: só volta por QR presencial, Q37).

### 3.4 Check-in no abrigo (funcionalidade 1)
O check-in é o momento em que a pessoa é conferida [dec. h, A1].

**Online, pessoa consciente:**
1. Voluntário toca "Check-in". O app mostra a tela de consentimento (finalidade, prazo de guarda, quem acessa e, se o abrigo é `unconfirmed`, o aviso "Abrigo não confirmado pela Defesa Civil" [dec. d]), com seletor próprio de idioma PT | EN | ES (11.3) [dec. h, M3]. O voluntário marca a base legal: `consent` (art. 11, I) ou `vital_interest` (art. 11, II, "e") se a pessoa estiver ferida ou incapacitada.
2. Captura com liveness (`purpose: "checkin"`, `shelter_id`).
3. `POST /v1/shelters/{id}/checkins {id, capture_id, data, legal_basis}`.
4. Servidor, em transação sobre `people/{person_id}`:
   - não existe: cria com `level = verified`;
   - existe com `level = assisted`: sobe para `verified` e mantém o histórico;
   - fecha a `stay` aberta em outro abrigo (`left_at = agora`, `exit_reason = checkin_elsewhere`) e abre a `stay` no abrigo atual;
   - procura buscas ativas com esse `person_id` (4.8) e cria matches.
5. Resposta: `{person_id, level, is_new, history}`. `history` traz as `stays` de outros abrigos só se o abrigo for `confirmed` [dec. d].
6. Cartão (3.5.1): o app oferece "Entregar cartão novo" ou "Adicionar a um cartão existente" (núcleo familiar que chegou junto).

**Sem liveness possível (ferido, inconsciente, criança pequena) ou offline:**
1. Foto pela câmera do app. O app reduz a imagem (6.1.4), guarda o `Blob` no IndexedDB e enfileira `POST /v1/shelters/{id}/checkins {id, photo_base64, data, legal_basis}`.
2. Enquanto não sincroniza, a pessoa aparece na lista local como "pendente". O cartão pode ser emitido offline (3.5.1).
3. No servidor, a foto vai ao flow `api+humantoken` e gera `level = assisted`. O servidor descarta a foto após a chamada.
4. Ao receber `2xx`, o app apaga o `Blob` [dec. a, e].
5. A pessoa sobe para `verified` na primeira captura com liveness [dec. i, Q1]. A lista "Pendentes de selfie" do abrigo mostra quem está `assisted`.
6. Erro de rosto (`20532`, inconclusivo): o item fica `needs_action` e o app pede outra foto. **PROPOSTA (a confirmar) (Q28):** se não houver como fotografar de novo, o voluntário registra "sem biometria" (`level = no_biometrics`, `id` = UUID do cliente); esse registro não entra na busca por foto.

**Pessoa sem identificação** [dec. e]: se o check-in ou o resgate chegar sem `name` e sem `wristband`, o app envia também `thumbnail_base64` (6.1.4) e o servidor guarda cifrada. Ver 4.6.

**Criança:** `data.is_child = true`. O voluntário associa responsáveis já cadastrados (`POST /v1/people/{id}/guardians`), que precisam ser `verified`. Base legal: `child_protection` (art. 14, § 3º).

### 3.5 Doações (funcionalidade 2) [dec. h, A1]

#### 3.5.1 Cartão do núcleo familiar
- Um `card` tem um código curto (`code`: 8 caracteres Crockford base32, gerado no celular, legível e digitável) mostrado como QR. Serve como cartão impresso, adesivo ou pulseira (pode ser improvisada com o código escrito à mão).
- Membros: uma ou mais pessoas (`person_ids`) do mesmo núcleo familiar, declarado no check-in. Uma pessoa está em no máximo um cartão ativo.
- Emissão: no check-in, online ou offline (`POST /v1/shelters/{id}/cards`). Perda: o voluntário cancela o cartão e emite outro (`POST /v1/cards/{code}/cancel`).
- O cartão vale em qualquer abrigo da rede.
- **PROPOSTA (a confirmar) (Q44):** formato do código e regra de uma pessoa por cartão ativo.

#### 3.5.2 Entrega do dia a dia
1. Admin configura os itens do abrigo: nome, período em dias e `high_value` (cartão-vale, colchão, eletrodoméstico) (`PUT /v1/shelters/{id}/items`). **PROPOSTA (a confirmar):** período padrão 7 dias (Q17); regra por pessoa (Q16).
2. Voluntário lê o QR com a câmera do app (ou digita o código). O app mostra os membros do cartão (cache local se offline) e o voluntário toca em quem recebe.
3. `POST /v1/shelters/{id}/deliveries {id, card_code, person_id, item, quantity, delivered_at}`.
4. Servidor decide se exige liveness:
   - item `high_value`;
   - sorteio: `sha256(delivery_id)` interpretado como número em [0, 1) menor que `DELIVERY_SAMPLE_RATE` (**PROPOSTA (a confirmar)**: 0,05, Q41);
   - pessoa com `level = assisted` (assim ela sobe para `verified`, 4.1).
   Se exige: `428 liveness_required`. O app faz captura com liveness (`purpose: "revalidation"`, `expected_person_id = person_id`) e reenvia com `capture_id`. O `person_id` da captura diferente do esperado → `409 revalidation_failed`, nada é gravado.
5. Servidor procura, em todos os eventos ativos [dec. c], entrega do mesmo `item` para o mesmo `person_id` dentro do período. Se houver: `status = in_review` com `reasons: [{kind: duplicate_in_period, previous_delivery_id}]`. **A entrega nunca é bloqueada** por duplicidade (histórico, ideia 2).
6. Resposta: "Entrega registrada" e, se sinalizada, "Enviada para revisão: outra entrega em <data>". Não mostra o abrigo da outra entrega a abrigo não confirmado.
7. Offline: o cartão funciona pelo cache. Entrega que exigiria liveness (`high_value` ou pessoa `assisted`) é gravada com `verification = card` e `status = in_review`, motivo `liveness_skipped_offline`. O sorteio não roda offline.
8. Admin revisa em `GET /v1/shelters/{id}/deliveries?status=in_review` e decide `reviewed_ok` ou `irregular`, com nota.

### 3.6 Registro de resgatados (funcionalidade 3) [dec. h, A11]
O registro é feito **no ponto de desembarque ou triagem**, não no barco. O ator é um voluntário que está nesse ponto.
1. Voluntário logado toca "Resgate". Foto só pela câmera do app.
2. O app grava junto: data e hora, GPS do ponto (`lat`, `lon`, `accuracy`), `volunteer_id`, `device_id`.
3. Campos opcionais: nome, pulseira, observação, nome do ponto de desembarque (`landing_point`), abrigo de destino (`destination_shelter_id`).
4. Sem nome e sem pulseira: miniatura (4.6).
5. Vai pela fila: `POST /v1/rescues`. No servidor, `api+humantoken` → `person_id`; cria a pessoa como `assisted` se não existe, ou acrescenta o resgate ao histórico. Evento: o do abrigo `open` mais próximo a ≤ `EVENT_RADIUS_KM` do GPS; senão nulo.
6. Quando a pessoa faz check-in em qualquer abrigo, o token é o mesmo e o resgate já aparece no histórico.
7. Matches com buscas ativas são procurados também aqui.
8. Regras de anomalia (4.10), com limiar de volume próprio para resgate [dec. h, A11].

### 3.7 Busca de desaparecidos (funcionalidade 5)
1. Pré-condição: voluntário de abrigo `confirmed`, ou DC [dec. d].
2. O operador registra: nome e observações do desaparecido; nome, telefone, parentesco e idioma da família; de 1 a 5 fotos (galeria permitida, é a exceção). **PROPOSTA (a confirmar) (Q8):** limite de 5 fotos por busca e 10 buscas por voluntário por dia.
3. `POST /v1/searches`. **PROPOSTA (a confirmar):** pode ir pela fila se offline (Q45).
4. Servidor gera um token por foto (`api+humantoken`), guarda só os `person_id` derivados (`searches.person_ids`, nível "busca": não vira pessoa), e compara com `people`. Um match em qualquer foto conta.
5. **A resposta é sempre a mesma**, com ou sem match: `202 {message: "search_registered", usable_photos}`. O app mostra "Busca registrada. Se a pessoa for encontrada e concordar, a equipe entrará em contato com a família." Nem o operador fica sabendo se houve match.
6. `usable_photos` diz quantas fotos geraram token, para o operador pedir outras. Não revela match.
7. **PROPOSTA (a confirmar) (Q25):** a busca fica ativa até o descarte do evento do abrigo de origem. Cada check-in, resgate ou "Estou Seguro" posterior é comparado com as buscas ativas.
8. Match → mediação (4.8).

### 3.8 "Estou Seguro" (funcionalidade 6)
1. Página pública `/safe`, sem login, em PT | EN | ES [dec. h, M3].
2. No topo, sempre visível: **"Precisa de resgate? Ligue 193 (Bombeiros) ou 199 (Defesa Civil)"**, com dois botões `tel:193` e `tel:199`. Nada é gravado por esse caminho [dec. h, C3].
3. Para se registrar, a pessoa lê o aviso (finalidade, prazo, base legal `consent`, art. 11, I) e faz captura com liveness (`purpose: "safe_check"`).
4. Escolhe: "estou seguro" (`safe`) ou "fui para um abrigo" (`went_to_shelter`, com lista opcional de abrigos abertos próximos). GPS opcional.
5. **PROPOSTA (a confirmar) (Q24):** a pessoa marca ou não "Se alguém da minha família me procurar, pode saber que estou seguro (sem dizer onde estou)" (`family_contact_allowed`).
6. `POST /v1/safe-check`. O servidor cria ou atualiza `people/{person_id}` com `level = verified` e o bloco `safe_check`. Procura buscas ativas.
7. Proteção de custo: 10.4.

### 3.9 Revalidação (funcionalidade 7)
Revalidação = nova captura com liveness cujo `person_id` resultante é comparado com o esperado (5.1).

**Entrega de doação:** só itens `high_value`, sorteio e pessoa `assisted` (3.5.2) [dec. h, A1].

**Saída de criança:**
1. Voluntário abre a criança e toca "Saída". Escolhe o responsável que está retirando (de `guardian_ids`).
2. Captura com liveness do adulto (`purpose: "revalidation"`, `expected_person_id = guardian_id`).
3. `POST /v1/shelters/{id}/exits {person_id, guardian_id, capture_id}`.
4. `person_id` da captura = `guardian_id`: saída registrada, `stay` fechada.
5. Diferente: `409 revalidation_failed`. **PROPOSTA (a confirmar) (Q18):** a saída não é registrada e um alerta vai aos admins; um admin pode registrar saída por exceção (`exception: {reason}`), auditada e com alerta `exceptional_exit` à DC.
6. Saída de adulto: sem revalidação, `POST /v1/shelters/{id}/exits {person_id}`.

### 3.10 Confirmação de abrigo [dec. d]
**Por validador:** `POST /v1/shelters/{id}/confirm`. `confirmation_type = validator`, `can_endorse = true`.

**Aval presencial:**
1. Admin do abrigo novo gera um código: `POST /v1/shelters/{id}/endorsements/in-person/code {lat, lon, accuracy}` → `{code, expires_at}`, mostrado em QR. **PROPOSTA (a confirmar):** expira em 10 min (Q38).
2. Admin avalista (de abrigo `confirmed` com `can_endorse`, sem vínculo com o abrigo novo) lê o QR com a câmera do celular, que abre `/endorse/<code>`: `POST /v1/endorsements/in-person {code, endorser_shelter_id, lat, lon, accuracy}`.
3. Servidor exige as duas `accuracy ≤ 100` e distância entre os dois celulares ≤ `SHELTER_RADIUS_M`. Confirma com `confirmation_type = in_person`, `can_endorse = true`, registra quem avalizou.

**Aval remoto:**
1. Admins de abrigos com `can_endorse`, do mesmo evento, veem `GET /v1/events/{id}/shelters-to-endorse`: fotos do local, endereço, nome do criador, distância.
2. `POST /v1/shelters/{id}/endorsements/remote {endorser_shelter_id}`.
3. Com dois avais de dois abrigos diferentes, por dois voluntários diferentes, nenhum ligado ao abrigo novo: `confirmed`, `confirmation_type = remote`, `can_endorse = false`.
4. "Ligado ao abrigo novo" = ter ou ter tido `membership` com ele (**PROPOSTA (a confirmar)**, Q30).

**Revogação:** validador ou DC, `POST /v1/shelters/{id}/revoke {reason}` → `unconfirmed`, `can_endorse = false`; cada voluntário que deu aval fica `flagged` e gera alerta `endorser_flagged`. Sem cascata (**PROPOSTA (a confirmar)**, Q20).

### 3.11 Painel da Defesa Civil
`GET /v1/dashboard/events` e `GET /v1/dashboard/events/{id}`: por evento, abrigos (nome, confirmação, `flagged`, posição), pessoas únicas por abrigo (`current_shelter_id`), total de pessoas únicas no evento, movimentações entre abrigos (`stays` fechadas por `checkin_elsewhere` nas últimas 24 h e 7 dias), resgates, alertas abertos, matches na fila DC, abrigos aguardando aval. Contagens por agregação `count()` do Firestore.

---

## 4. Regras de negócio

### 4.1 Níveis de confiança das pessoas

| `level` | Origem | Libera | Transições |
|---|---|---|---|
| `verified` | Captura com liveness (check-in, entrega revalidada, "Estou Seguro") | Abrigo, doação, ser responsável por criança | final |
| `assisted` | Foto pela câmera do app (resgate, check-in offline ou sem liveness) | Localização e reunificação; doação só com captura com liveness (que sobe o nível) ou em revisão se offline | → `verified` na primeira captura com liveness, sem cadastro novo [dec. i, Q1] |
| `no_biometrics` | Registro manual sem token (PROPOSTA, Q28) | Presença no abrigo | nenhuma |
| "busca" | Foto da família | Nada; fica só em `searches.person_ids` | não vira pessoa |

Nível nunca desce.

### 4.2 Abrigo
- `status`: `open` → `closed` (admin ou DC); `open` → `merged` (fusão aceita).
- `confirmation`: 2.2 e 3.10.
- `unconfirmed` vê só o que ele mesmo cadastrou; pessoas cadastradas nele entram normalmente na busca da rede [dec. d].
- Raio mínimo entre abrigos abertos: `SHELTER_RADIUS_M` (padrão 100) [dec. a].
- Precisão exigida: `accuracy ≤ 100 m` na criação e nos avais presenciais.

### 4.3 Eventos automáticos [dec. c]
Na criação de um abrigo online (ou na sincronização de um criado offline):
1. Se o pedido trouxe `declared_event_ids`, entra no primeiro evento declarado e `active` (`event_link = declared`). **PROPOSTA (a confirmar) (Q31):** participa da regra de cadeia como qualquer outro.
2. Senão, procura abrigos `open` de eventos `active` a ≤ `EVENT_RADIUS_KM` (padrão 50):
   - nenhum: cria evento com nome automático "Cidade/UF – mês/ano"; sem endereço ainda, "Evento – mês/ano", refeito quando o endereço chegar se `auto_named` ainda for `true`;
   - um evento: entra nele;
   - dois ou mais: **fusão de eventos**. Sobrevive o mais antigo; os outros ficam `status = merged`, `merged_into = <sobrevivente>`; abrigos, buscas, resgates e `people.event_ids` são reapontados em lotes.
3. Admin de qualquer abrigo do evento renomeia (`PATCH /v1/events/{id}`) → `auto_named = false`.
4. Dois desastres próximos viram um evento só [dec. c].
5. Atividade: qualquer escrita em abrigo do evento atualiza `events.last_activity_at`, no máximo uma vez por hora (cache em memória).
6. Encerramento automático (tarefa horária, 6.2.6): evento `active` com todos os abrigos `closed`/`merged`, ou com `agora − last_activity_at ≥ INACTIVITY_DAYS` (padrão 30) → `closed`, `closed_at = agora`, `purge_at = agora + PURGE_AFTER_DAYS`.
7. Evento `closed` não reabre. **PROPOSTA (a confirmar) (Q36):** abrigo de evento encerrado não aceita escritas (`409 event_closed`).
8. O evento organiza mas não isola: busca e duplicidade de doação olham todos os eventos ativos.

### 4.4 Convites e membros
Regras em 3.3 e 2.3. Remoção vale só para aquele abrigo e gera alerta `volunteer_removed` para os admins dos outros abrigos em que a pessoa é membro ativo [dec. b]. Banimento (DC): `volunteers.banned = true`, sessões apagadas, todas as `memberships` `removed`.

### 4.5 Raio de 100 m
- Distância por haversine sobre `lat/lon`; candidatos por geohash (6.3.3).
- Usado em: criação de abrigo e aval presencial.

### 4.6 Miniatura de pessoa sem identificação [dec. e]
- Gerada no celular: 128×128 px, JPEG qualidade 0,6 (~5–10 KB).
- Enviada junto do check-in ou resgate quando não há `name` nem `wristband`.
- No servidor: AES-256-GCM (`THUMBNAIL_KEY`, nonce aleatório de 12 bytes), campo `people.thumbnail_encrypted`.
- Nunca é usada para gerar token.
- Quem vê (`GET /v1/people/{id}/thumbnail`): membros ativos do `current_shelter_id`, o mediador de um match aberto sobre ela e — **PROPOSTA (a confirmar) (Q33)** — se a pessoa ainda não tem abrigo, o voluntário do ponto de desembarque que registrou o resgate e a DC. Cada acesso grava auditoria.
- Apagada na mesma escrita que preencher `name` ou `wristband`, ou quando todos os eventos da pessoa estiverem encerrados. O que vier primeiro.

### 4.7 Doações
- Chave da duplicidade: `(person_id, item)`. Janela: `items[item].period_days` do abrigo que entrega. Busca em todos os eventos ativos.
- Duplicidade → `in_review`, nunca bloqueio.
- Liveness na entrega só para `high_value`, sorteio e pessoa `assisted` [dec. h, A1].
- `verification`: `card` (padrão), `liveness` (revalidada). Entrega offline que exigiria liveness → `in_review`, motivo `liveness_skipped_offline`.

### 4.8 Mediação da busca
**Nunca devolver localização a quem buscou.** Ao criar um match (`matches/{sha256(search_id|person_id)}`, idempotente), o servidor escolhe o mediador:

| Situação da pessoa encontrada | `mediator_kind` |
|---|---|
| `current_shelter_id` `confirmed` | `shelter` (admins desse abrigo) |
| `current_shelter_id` `unconfirmed` | `civil_defense` [dec. d] (**PROPOSTA**: não um abrigo confirmado próximo, Q24) |
| Sem abrigo (só resgate) | `civil_defense` |
| Só "Estou Seguro", com `family_contact_allowed` | `search_origin`: admins do abrigo que registrou a busca, que recebem só "declarou-se segura em <data>" e o contato da família (PROPOSTA, Q24) |
| Só "Estou Seguro", sem consentimento prévio | `none`; match gravado `declined_in_advance` |

Fluxo do mediador:
1. Vê o aviso em "Avisos" (`GET /v1/matches`): nome do desaparecido segundo a família, nome da pessoa no cadastro, miniatura se houver, abrigo. **Não vê o contato da família ainda.**
2. Conversa com a pessoa. A pessoa decide; se estiver incapacitada, o mediador decide (`decided_by = operator`).
3. `POST /v1/matches/{id}/decision {decision: accept|decline, decided_by: person|operator, note}`.
4. `accept`: a resposta traz `family {name, phone, relationship, language}` e `whatsapp_link` (`https://wa.me/<phone>?text=<mensagem>`, no idioma da família). Localização nunca é enviada automaticamente.
5. `decline`: nada é enviado; a busca continua ativa para outras pessoas, sem novo match para esse `person_id`.
6. Toda decisão grava auditoria com `legal_basis = vital_interest`.

### 4.9 Criança
- `is_child = true` exige `guardian_ids` `verified` para saída com revalidação.
- `legal_basis = child_protection` (art. 14, § 3º).

### 4.10 Auditoria e anomalias
Toda escrita e todo acesso a miniatura ou a dados de match gravam `audit_log`. Anomalias geram alerta, sem bloquear. **PROPOSTA (a confirmar) (Q39)** para os limiares:
- `anomaly_volume`: check-ins do mesmo voluntário acima de `ANOMALY_CHECKINS_PER_10MIN` (padrão 30); resgates acima de `ANOMALY_RESCUES_PER_10MIN` (padrão 100, porque muitos registros seguidos são normais no ponto de desembarque [dec. h, A11]).
- `anomaly_distance`: o mesmo `person_id` registrado em dois pontos a mais de 50 km com menos de 1 h de diferença.
- `out_of_area`: resgate a mais de `EVENT_RADIUS_KM` de todos os abrigos onde o voluntário é membro.
Alertas vão à DC e aos admins dos abrigos do voluntário.

---

## 5. Integração com a Unico

### 5.1 Flows usados

| Tela | Flow | Nível |
|---|---|---|
| Login, convite, check-in online, revalidação (entrega e saída de criança), "Estou Seguro" | `humantoken` (Journey, com liveness) | `verified` |
| Check-in sem liveness ou offline, resgate | `api+humantoken` (sem liveness) | `assisted` |
| Busca da família | `api+humantoken` | busca |
| — | `api+liveness+humantoken*`: **não usado**; rejeita JPEG comum com `400 INVALID_CAPTURE_TOKEN` (teste de 2026-10-20) | — |
| — | `humantoken+revalidation`, `api+liveness+humantoken+revalidation`: **não usados** (13) | — |

**Revalidação sem o flow de revalidação.** Nova jornada `humantoken` e comparação do `person_id` derivado. Mesmo resultado e custo do flow `+revalidation`, sem guardar o token puro (6.3.2).

**Jornada em vez de SDK** [dec. i, Q4]: liveness pela jornada da Unico (página aberta pelo celular). **Primeira tarefa da implementação:** testar a jornada a partir da PWA instalada no iPhone (navegação para fora do escopo do app em modo standalone, permissão de câmera, volta pelo `callbackUri`). Plano B: SDK web da Unico.

### 5.2 Jornada (`humantoken`)
1. `POST /v1/captures` → servidor chama `POST {UNICO_URL}/process` com `{"flow": "humantoken", "callbackUri": "https://<domain>/return?capture=<capture_id>"}`. Guarda `process_id = id` e devolve `link`. Captura expira em 15 min.
2. O endpoint que consome a captura chama `GET {UNICO_URL}/process/{process_id}` se ela ainda não foi resolvida (polling não cria processo, não custa).
3. Estados:
   - `PROCESS_STATE_CREATED`: `202 capture_pending`; o app tenta de novo em 2 s, 4 s, 8 s, até 2 min.
   - `PROCESS_STATE_FINISHED` com `data.humanToken.value` **não vazio**: sucesso.
   - `PROCESS_STATE_FINISHED` sem `value` ou com `value` vazio: `422 capture_inconclusive`.
   - `PROCESS_STATE_FAILED`: `422 capture_failed`.
   - `PROCESS_STATE_CANCELED`: `422 capture_canceled`.
   - outros (`UNSPECIFIED`, `AWAITING_FOR_DOCUMENT`): `502 unico_unexpected_state`, com log.
4. A captura é de uso único (`consumed = true` na mesma transação que grava o resultado) e só pode ser consumida pelo `device_id` que a criou.

### 5.3 API Only (`api+humantoken`)
- Corpo: `{"flow": "api+humantoken", "imageBase64": "<base64>"}`. PNG/JPEG/WebP ≤ 800 KB; acima disso o servidor responde `413 image_too_large` sem chamar a Unico.
- Sucesso: `data.humanToken.value` não vazio.
- Sem rosto (`20532`): `422 no_face`. `value` vazio ou `PROCESS_STATE_FAILED`: `422 capture_inconclusive`.
- `400 INVALID_IMAGE`, `UNSUPPORTED_IMAGE_FORMAT`, `IMAGE_TOO_LARGE`, `MISSING_IMAGE`: `422 invalid_image`.

### 5.4 Regra do `value` vazio
Uma função única no cliente Unico (`extract_token`) devolve `Option<String>` e trata ausente, string vazia e só espaços como "sem token". Nenhum outro código lê `data.humanToken.value`.

### 5.5 Erros, 429 e backoff
- `429` com mensagem `monthly process limit ... reached`: não repetir. `503 unico_limit`, alerta `unico_limit` para a DC, log `error`.
- `429` sem essa mensagem: backoff exponencial com jitter no servidor (1, 2, 4, 8 s + 0–1 s aleatório), no máximo 4 tentativas. Depois, `503 unico_busy` com `Retry-After: 30`; a fila do app reagenda (9.2).
- `500`/timeout (60 s): uma nova tentativa após 2 s; depois `502 unico_unavailable`.
- `401`/`403 PERMISSION_DENIED`: `500 unico_misconfigured` e log `error`.
- Concorrência: semáforo de 4 chamadas simultâneas à Unico no processo.
- Não há sandbox: toda chamada é produção. Testes automatizados usam um servidor falso (`UNICO_URL`); chamadas reais só em teste manual.

### 5.6 Chave
`HUMAN_TOKEN_ACCESS_KEY` (formato `uk_live_...`) só no arquivo de ambiente da VM (6.4) ou no `.env` local. Nunca vai ao celular [dec. a, g]. Rotação pela API da Unico invalida a antiga na hora: atualizar o vault, rodar o `configure.yml` e reiniciar o serviço.

---

## 6. Arquitetura e implantação

```
Celular (PWA TypeScript) ──HTTPS──► Caddy (VM e2-micro)
   │  IndexedDB + fila                 ├── /           → arquivos estáticos da PWA
   │                                   └── /v1/*       → API Rust (127.0.0.1:8080)
   ├──► Nominatim (só endereço)                         ├──► Firestore (mesma região da VM)
   └──► Jornada Unico (só captura com liveness)         └──► Unico Human Token API
```
O celular nunca fala com o Firestore nem com a API da Unico; só abre a página da jornada [dec. g].

**Região por instalação** [dec. h, A8]: a região do GCP é configuração de cada instalação (`gcp_region`/`gcp_zone` no inventário Ansible). O piloto roda em `us-east1` (zona `us-east1-b`), que tem a camada gratuita da e2-micro e fica mais perto do Brasil que `us-central1` [dec. g]. Uma prefeitura pode subir a própria instância em `southamerica-east1` (São Paulo), fora da camada gratuita; o RIPD documenta isso (10.1.4). Domínio do piloto: `taba.thingsbuilder.com`.

### 6.1 PWA
- **Stack:** Vite + TypeScript + Preact + `idb` + `vite-plugin-pwa` (Workbox, app shell em cache). Roteamento por `location.pathname` com um `switch` simples.
- **QR:** geração com `qrcode`; leitura com `BarcodeDetector` quando existe e `jsQR` como reserva (Safari no iOS não tem `BarcodeDetector`).
- **Rotas:** `/`, `/login`, `/return`, `/shelters/new`, `/shelter/:id`, `/shelter/:id/checkin`, `/shelter/:id/people`, `/shelter/:id/delivery`, `/shelter/:id/exit`, `/shelter/:id/members`, `/person/:id`, `/rescue`, `/search`, `/inbox`, `/invite/:code`, `/safe`, `/dashboard`, `/endorse/:code`, `/privacy`.

#### 6.1.1 IndexedDB (`taba`, versão 1)
| Store | Chave | Conteúdo |
|---|---|---|
| `config` | nome | `device_id`, `language`, `session` (segredo + `volunteer_id` + `expires_at`) |
| `queue` | `id` (UUID) | `{method, path, body, volunteer_id, shelter_id?, created_at, attempts, next_attempt_at, state: pending\|held\|needs_action, error?}` |
| `blobs` | `queue_id` | Fotos pendentes (`Blob`), apagadas quando o item recebe `2xx` |
| `shelters` | `id` | Abrigos do voluntário (cache) |
| `people` | `id` | Pessoas do abrigo atual (cache offline por nome/pulseira), incluindo pendentes locais |
| `cards` | `code` | Cartões do abrigo atual e seus membros (cache offline) |
| `captures` | `capture_id` | Captura em andamento (para retomar após voltar da jornada) |
| `addresses` | `shelter_id` | Coordenadas aguardando Nominatim |

#### 6.1.2 Persistência
`navigator.storage.persist()` no primeiro uso logado; resultado mostrado em "Configurações".

#### 6.1.3 Câmera
`getUserMedia({video: {facingMode: "user" | "environment"}})`, quadro em `canvas`. Sem `<input type="file">` no check-in e no resgate. Na busca, `<input type="file" accept="image/*" multiple>`.

#### 6.1.4 Redução de imagem
`canvas` → `toBlob("image/jpeg", q)`:
- Foto para token: maior lado 1280 px; qualidade começa em 0,85 e cai 0,1 até ≤ 300 KB (**PROPOSTA (a confirmar)**, Q34). O tráfego de saída acima de 1 GB/mês é custo conhecido de um evento real (~US$ 0,12/GB) [dec. i, Q5].
- Miniatura: 128×128, qualidade 0,6.
- Foto do local: maior lado 1024 px, ≤ 200 KB.

#### 6.1.5 Geolocalização
`getCurrentPosition` com `enableHighAccuracy: true`; grava `accuracy` sempre.

#### 6.1.6 Nominatim
`GET https://nominatim.openstreetmap.org/reverse?format=jsonv2&lat=..&lon=..&accept-language=pt-BR`, no máximo 1 requisição por segundo (política do Nominatim), resultado guardado no IndexedDB. Sem sinal: guarda em `addresses` e tenta quando voltar; ao obter, `PATCH /v1/shelters/{id} {address}` pela fila.

### 6.2 API Rust
Um binário `taba` com dois subcomandos: `serve` (API + tarefa horária) e `role` (CLI de papéis globais).

**Crates:** `axum`, `tokio`, `tower-http` (limite de corpo 3 MB, trace), `serde`/`serde_json`, `reqwest` (rustls), `firestore` (gRPC; autenticação pela conta de serviço da VM via servidor de metadados; emulador por `FIRESTORE_EMULATOR_HOST` nos testes), `geohash`, `hmac` + `sha2`, `aes-gcm`, `rand`, `base64`, `uuid`, `time`, `governor` (limite por IP), `tracing` + `tracing-subscriber`, `anyhow`.

#### 6.2.1 Módulos
`main.rs` (CLI e roteador), `config.rs`, `auth.rs` (sessões, extrator de voluntário, checagem de papel), `unico.rs`, `geo.rs` (geohash e haversine), `shelters.rs`, `events.rs`, `members.rs` (convites e memberships), `people.rs` (check-in, saídas, miniatura), `cards.rs`, `deliveries.rs`, `rescues.rs`, `searches.rs` (buscas e matches), `safe_check.rs`, `dashboard.rs`, `jobs.rs` (encerramento e descarte), `audit.rs` (auditoria e anomalias), `idempotency.rs`.

#### 6.2.2 Configuração (variáveis de ambiente)
Na VM, lidas de `/etc/taba/env`; em desenvolvimento, de `.env` criado a partir de `.env.example` (6.4).

| Variável | Padrão | Uso |
|---|---|---|
| `HUMAN_TOKEN_ACCESS_KEY` | — | Chave Unico |
| `UNICO_URL` | `https://humantoken.unico.app/client/v1` | Trocável nos testes |
| `DOMAIN` | — | Monta `callbackUri` e links |
| `GCP_PROJECT` | — | Firestore (já gerada pelo `env.j2` atual) |
| `HMAC_KEY` | — | 32 bytes base64; deriva `person_id`/`volunteer_id` [dec. i, Q3] |
| `THUMBNAIL_KEY` | — | 32 bytes base64; AES-GCM |
| `SHELTER_RADIUS_M` | 100 | [dec. a] |
| `EVENT_RADIUS_KM` | 50 | [dec. c] |
| `INACTIVITY_DAYS` | 30 | [dec. c] |
| `PURGE_AFTER_DAYS` | 30 | **PROPOSTA (a confirmar)**, Q9 |
| `SESSION_DAYS` | 7 | **PROPOSTA (a confirmar)**, Q26 |
| `PUBLIC_CAPTURES_PER_DAY` | 500 | **PROPOSTA (a confirmar)**, Q15 |
| `DELIVERY_SAMPLE_RATE` | 0.05 | **PROPOSTA (a confirmar)**, Q41 |
| `ANOMALY_CHECKINS_PER_10MIN` | 30 | **PROPOSTA (a confirmar)**, Q39 |
| `ANOMALY_RESCUES_PER_10MIN` | 100 | **PROPOSTA (a confirmar)**, Q39 |

#### 6.2.3 Formato de erro
`{"error": "<stable_code>", "detail": {...}}`. O app traduz o código; o servidor não traduz texto. Mensagens de log em inglês [dec. j].

#### 6.2.4 Concorrência e unicidade
- **Unicidade de pessoa e voluntário:** pelo ID do documento (6.3.2) em transação `get` → `create`/`update` [dec. g].
- **Criação de abrigo e fusão de eventos:** um `tokio::sync::Mutex` global no processo. `// ponytail: global lock; fine with the single e2-micro instance. With more than one instance, switch to a transaction on a per-geohash-cell lock document.`
- **Matches:** ID determinístico (`sha256(search_id|person_id)`) e `create` que falha se já existe.
- **Cartões:** `cards/{code}` criado com `create` (falha se o código já existe; o app gera outro). Uma pessoa por cartão ativo: verificado na mesma transação.

#### 6.2.5 Autenticação
- Sessão: segredo de 32 bytes aleatórios (base64url) devolvido ao app; no Firestore, `sessions/{sha256(secret)}`. Cabeçalho `Authorization: Bearer <secret>`.
- Expira em `SESSION_DAYS`, renovada a cada requisição autenticada com mais de 1 dia desde a última renovação.
- Banimento apaga as sessões do voluntário.
- O extrator `Volunteer` carrega perfil e `memberships` ativas; cada handler checa papel e estado do abrigo pela matriz 2.3.

#### 6.2.6 Tarefa horária (encerramento e descarte)
Tarefa `tokio` dentro do `serve`, a cada hora:
1. Encerra eventos (4.3.6).
2. Apaga miniaturas de pessoas cujos eventos estão todos encerrados.
3. Descarta eventos com `purge_at ≤ agora` (10.1.2), em lotes de 400 escritas, retomando na hora seguinte se passar de 15 000 exclusões no dia (camada gratuita: 20 000/dia).
4. Expira capturas, convites e códigos de aval vencidos (os que não usam TTL).

### 6.3 Firestore
O banco é o `(default)`, em modo nativo, na região da instalação — o único que usa a cota gratuita (`provision.yml`).

#### 6.3.1 Coleções — seção 7.

#### 6.3.2 Identificadores derivados do token [dec. i, Q3]
`person_id = volunteer_id = base64url(HMAC-SHA256(HMAC_KEY, token))` (43 caracteres). O token puro não é gravado em lugar nenhum. A unicidade continua sendo pelo ID do documento [dec. g]. Motivos: o formato do token não é documentado e pode conter `/` (proibido em ID do Firestore); o token é o mesmo em qualquer cliente da Unico, e o HMAC impede cruzar bases em caso de vazamento; a revalidação compara IDs derivados.
`HMAC_KEY` é segredo próprio, no vault de cada instalação, com backup fora da VM: perdê-la torna todos os IDs incomparáveis com tokens novos.

#### 6.3.3 Geohash
- Campo `geohash` (precisão 9) em `shelters`.
- Consulta "a ≤ r metros": escolher a maior precisão `p` cuja célula tenha o menor lado ≥ r (p = 7 para 100 m; p = 3 para 50 km), calcular a célula do ponto e as 8 vizinhas, e fazer 9 consultas de intervalo (`geohash >= prefix` e `< prefix + "~"`) com `status == "open"`. Filtrar por haversine.

#### 6.3.4 Índices compostos (`infra/firestore/firestore.indexes.json`)
| Coleção | Campos |
|---|---|
| `shelters` | `status` ASC, `geohash` ASC |
| `shelters` | `event_id` ASC, `status` ASC |
| `people` | `current_shelter_id` ASC, `updated_at` DESC |
| `cards` | `shelter_id` ASC, `status` ASC |
| `cards` | `person_ids` ARRAY_CONTAINS, `status` ASC |
| `deliveries` | `person_id` ASC, `item` ASC, `delivered_at` DESC |
| `deliveries` | `shelter_id` ASC, `status` ASC, `delivered_at` DESC |
| `searches` | `person_ids` ARRAY_CONTAINS, `status` ASC |
| `matches` | `mediator_kind` ASC, `mediator_shelter_id` ASC, `status` ASC, `created_at` DESC |
| `memberships` | `volunteer_id` ASC, `status` ASC |
| `memberships` | `shelter_id` ASC, `status` ASC |
| `alerts` | `resolved` ASC, `created_at` DESC |
| `audit_log` | `shelter_id` ASC, `at` DESC |
| `audit_log` | `volunteer_id` ASC, `at` DESC |

Políticas de TTL: `sessions.expires_at`, `captures.expires_at`, `idempotency.expires_at`, `audit_log.expires_at`.

#### 6.3.5 Cotas da camada gratuita (referência)
1 GiB armazenado; 50 000 leituras, 20 000 escritas e 20 000 exclusões por dia. Cada operação = 1 escrita de negócio + 1 de auditoria.

### 6.4 VM e implantação
A infraestrutura é criada e configurada com Ansible em `infra/ansible/`: `provision.yml` (APIs, Firestore, conta de serviço só com `roles/datastore.user`, IP estático, firewall 22/80/443, VM e2-micro com Debian 13 e disco `pd-standard` de 30 GB) e `configure.yml` (atualizações automáticas, Caddy, `/etc/taba/env`). O passo a passo está em **`infra/README.md`**; esta spec não o repete.

- **Inventário:** `infra/ansible/inventory/example/` (no git, valores de exemplo) e `inventory/prod/` (fora do git, valores e vault de cada instalação). Nenhum valor de instalação (projeto, IP, chaves) entra no repositório.
- **Segredos:** na VM, `/etc/taba/env` (modo 0600, diretório 0700) gerado pelo template `env.j2` a partir do vault da instalação. O deploy da API acrescenta ao `env.j2` e ao `vault.yml.example` as variáveis da 6.2.2 que faltam (`HMAC_KEY`, `THUMBNAIL_KEY`, `DOMAIN`, limites), com chaves de vault `vault_taba_hmac_key` e `vault_taba_thumbnail_key`. Em desenvolvimento, `.env` a partir de `.env.example` (que passa a listar todas as variáveis, sem valores).
- **Caddy:** hoje responde só um placeholder (`Caddyfile.j2`). O deploy da API troca por: `file_server` para a PWA em `/srv/taba/pwa` e `reverse_proxy /v1/* 127.0.0.1:8080`, com os cabeçalhos `Strict-Transport-Security`, `Content-Security-Policy` (`default-src 'self'; connect-src 'self' https://nominatim.openstreetmap.org; img-src 'self' blob: data:`), `X-Content-Type-Options`, `Referrer-Policy: same-origin`, `Permissions-Policy: camera=(self), geolocation=(self)`.
- **systemd:** `taba.service` (`ExecStart=/usr/local/bin/taba serve`, `EnvironmentFile=/etc/taba/env`, `Restart=always`, `DynamicUser=yes`), instalado por uma tarefa nova do `configure.yml`.
- **Swap:** 1 GB (a VM tem 1 GB de RAM), também pelo `configure.yml`.
- **Build:** o binário é compilado fora da VM (`x86_64-unknown-linux-gnu`) e copiado; a e2-micro não tem memória para compilar Rust.
- A API confia em `X-Forwarded-For` só vindo de `127.0.0.1`.
- Custo: camada gratuita, menos o IPv4 externo (~US$ 3/mês) [dec. g] e a saída acima de 1 GB/mês [dec. i, Q5].

---

## 7. Modelo de dados (Firestore)

Datas: `Timestamp`. `client_*` = hora do celular; sem prefixo = hora do servidor.

### `events/{event_id}` (UUID do servidor)
`name`, `auto_named: bool`, `status: active|closed|merged|purged`, `merged_into?`, `created_at`, `last_activity_at`, `closed_at?`, `closure_reason?: all_shelters_closed|inactivity`, `purge_at?`, `purged_at?`, `stats?: {shelters, unique_people, rescues, deliveries}` (gravadas no descarte).

### `shelters/{shelter_id}` (UUID do cliente)
`name`, `lat`, `lon`, `accuracy_m`, `geohash`, `address: {text, city, state, pending: bool}`, `event_id?`, `event_link: auto|declared`, `created_by`, `client_created_at`, `created_at`, `status: open|closed|merged`, `merged_into?`, `confirmation: unconfirmed|confirmed`, `confirmation_type?: validator|in_person|remote`, `can_endorse: bool`, `endorsements: [{kind: in_person|remote, volunteer_id, endorser_shelter_id?, at, distance_m?}]`, `revocations: [{by, at, reason}]`, `flagged: bool`, `other_site_reason?`, `conflicts_with?`, `items: {<name>: {period_days, high_value: bool}}`, `photo_count`.

Subcoleção `shelters/{id}/photos/{n}`: `image: bytes`, `content_type`, `uploaded_by`, `uploaded_at`.

### `volunteers/{volunteer_id}` (HMAC do token)
`name`, `phone?`, `show_phone: bool`, `language: pt-BR|en`, `global_roles: [validator|civil_defense]`, `banned: bool`, `banned_by?`, `banned_at?`, `flagged: bool`, `flag_reasons: []`, `created_at`, `last_seen_at`.

### `memberships/{shelter_id}_{volunteer_id}`
`shelter_id`, `volunteer_id`, `role: admin|volunteer`, `status: active|removed|left`, `via: creator|qr|link|merge`, `invite_code?`, `invited_by?`, `joined_at`, `removed_by?`, `removed_at?`.

### `invites/{code}` (128 bits aleatórios, base64url)
`shelter_id`, `kind: qr|link`, `role: admin|volunteer` (`link` ⇒ `volunteer`), `created_by`, `created_at`, `expires_at`, `status: active|used|canceled`, `used_by?`, `used_at?`.

### `endorsement_codes/{code}`
`shelter_id`, `created_by`, `lat`, `lon`, `accuracy_m`, `expires_at`, `used: bool`.

### `sessions/{sha256(secret)}`
`volunteer_id`, `device_id`, `created_at`, `renewed_at`, `expires_at` (TTL).

### `captures/{capture_id}` (UUID do servidor)
`process_id` (Unico), `purpose: login|invite|checkin|revalidation|safe_check`, `device_id`, `volunteer_id?`, `shelter_id?`, `expected_person_id?`, `status: created|ok|inconclusive|failed|canceled`, `person_id?` (HMAC; nunca o token), `consumed: bool`, `created_at`, `expires_at` (15 min, TTL).

### `people/{person_id}` (HMAC do token; UUID do cliente se `no_biometrics`)
`level: verified|assisted|no_biometrics`, `name?`, `wristband?`, `is_child: bool`, `birth_year?`, `needs?` (texto livre), `guardian_ids: []`, `card_code?`, `current_shelter_id?`, `stays: [{shelter_id, event_id, entered_at, left_at?, exit_reason?: checkin_elsewhere|exit|exceptional_exit, registered_by}]`, `event_ids: []`, `rescue_ids: []`, `thumbnail_encrypted?: bytes`, `safe_check?: {status: safe|went_to_shelter, reported_shelter_id?, lat?, lon?, family_contact_allowed: bool, at}`, `origin: checkin|rescue|safe_check`, `legal_basis`, `created_at`, `updated_at`, `client_edited_at`.

### `cards/{code}` (8 caracteres Crockford base32, gerado no cliente)
`shelter_id` (emissor), `person_ids: []`, `status: active|canceled`, `issued_by`, `client_issued_at`, `created_at`, `canceled_at?`, `replaced_by?`.

### `deliveries/{delivery_id}` (UUID do cliente)
`shelter_id`, `event_id`, `person_id`, `card_code?`, `item`, `quantity`, `delivered_by`, `delivered_at` (cliente), `received_at`, `verification: card|liveness`, `status: ok|in_review|reviewed_ok|irregular`, `reasons: [{kind: duplicate_in_period|liveness_skipped_offline, previous_delivery_id?}]`, `reviewed_by?`, `review_note?`.

### `rescues/{rescue_id}` (UUID do cliente)
`person_id`, `volunteer_id`, `device_id`, `lat`, `lon`, `accuracy_m`, `landing_point?`, `captured_at` (cliente), `received_at`, `event_id?`, `destination_shelter_id?`, `note?`.

### `searches/{search_id}` (UUID do cliente)
`created_by`, `origin_shelter_id?`, `event_id?`, `person_ids: []` (HMAC dos tokens das fotos), `photos_sent`, `usable_photos`, `missing_person: {name, notes}`, `family: {name, phone, relationship, language}`, `status: active|closed`, `created_at`.

### `matches/{sha256(search_id|person_id)}`
`search_id`, `person_id`, `mediator_kind: shelter|civil_defense|search_origin|none`, `mediator_shelter_id?`, `status: open|accepted|declined|declined_in_advance`, `decided_by?: person|operator`, `decided_by_volunteer?`, `decided_at?`, `note?`, `created_at`.

### `merges/{merge_id}`
`source_shelter_id`, `target_shelter_id`, `requested_by`, `status: pending|accepted|declined`, `decided_by?`, `created_at`, `decided_at?`.

### `alerts/{alert_id}`
`kind: anomaly_volume|anomaly_distance|out_of_area|volunteer_removed|shelter_flagged|unico_limit|endorser_flagged|exceptional_exit`, `target_kind`, `target_id`, `event_id?`, `shelter_ids: []`, `for_civil_defense: bool`, `detail`, `created_at`, `resolved: bool`, `resolved_by?`.

### `audit_log/{auto}`
`at`, `volunteer_id?`, `device_id?`, `ip?`, `shelter_id?`, `action`, `target_kind`, `target_id`, `legal_basis?`, `lat?`, `lon?`, `expires_at` (TTL; **PROPOSTA**: 1 ano, Q21). Sem nome nem foto.

### `idempotency/{key}`
`volunteer_id?`, `route`, `http_status`, `body`, `expires_at` (7 dias, TTL).

`legal_basis` (enum): `consent` (art. 11, I) | `vital_interest` (art. 11, II, "e") | `fraud_prevention` (art. 11, II, "g") | `public_policy` (art. 11, II, "b") | `child_protection` (art. 14, § 3º) | `family_consent` (art. 7º, I).

---

## 8. API REST

Base `https://<domain>/v1`. JSON. Autenticação `Authorization: Bearer <session>` exceto onde indicado "público". Rotas que escrevem aceitam `Idempotency-Key` (o app usa o `id` do item da fila) e `X-Device-Id`; chave repetida devolve a resposta gravada em `idempotency`.

### 8.1 Capturas e sessão
| Método e rota | Auth | Corpo | Resposta |
|---|---|---|---|
| `POST /captures` | público para `login`, `invite`, `safe_check`; sessão para as demais | `{purpose, shelter_id?, expected_person_id?}` | `201 {capture_id, link, expires_at}`; `429 ip_limit`; `503 public_daily_limit` |
| `POST /sessions` | público | `{capture_id, profile?: {name, phone?, show_phone, language}}` | `201 {session, expires_at, volunteer}`; `202 capture_pending`; `422 profile_required`; `403 banned` |
| `DELETE /sessions/current` | sessão | — | `204` |
| `GET /me` | sessão | — | `{volunteer, memberships: [{shelter_id, name, role, confirmation}]}` |
| `PATCH /me` | sessão | `{name?, phone?, show_phone?, language?}` | `200` |

### 8.2 Abrigos e eventos
| Método e rota | Corpo | Resposta |
|---|---|---|
| `POST /shelters` | `{id, name, lat, lon, accuracy, address?, other_site_reason?, declared_event_ids?: [], site_photos?: [base64], client_created_at}` | `201 {shelter, event?}`; `409 shelter_nearby {shelter_id, name, distance_m, contact}`; `422 insufficient_accuracy` |
| `GET /shelters/{id}` | — | abrigo (membros ativos; outros voluntários veem só nome e confirmação) |
| `PATCH /shelters/{id}` | `{name?, address?}` (admin) | `200` |
| `POST /shelters/{id}/close` | — (admin ou DC) | `200` |
| `PUT /shelters/{id}/items` | `{items: {<name>: {period_days, high_value}}}` (admin) | `200` |
| `POST /shelters/{id}/photos` | `{image_base64}` (admin, máx. 2) | `201` |
| `GET /shelters/{id}/photos/{n}` | — (admin do abrigo, avalista elegível, Val, DC) | `image/jpeg` |
| `GET /events/nearby?lat&lon` | — | `[{event_id, name}]` |
| `PATCH /events/{id}` | `{name}` | `200` |
| `POST /shelters/{id}/merges` | `{target_shelter_id}` (admin da origem) | `201 {merge_id}` |
| `POST /merges/{id}/decision` | `{decision: accept|decline}` (admin do destino, abrigo `confirmed`) | `200` |

### 8.3 Confirmação
| Método e rota | Corpo | Resposta |
|---|---|---|
| `POST /shelters/{id}/confirm` | — (Val, DC) | `200` |
| `POST /shelters/{id}/revoke` | `{reason}` (Val, DC) | `200` |
| `POST /shelters/{id}/endorsements/in-person/code` | `{lat, lon, accuracy}` (admin do abrigo novo) | `201 {code, expires_at}` |
| `POST /endorsements/in-person` | `{code, endorser_shelter_id, lat, lon, accuracy}` | `200 {shelter}`; `403 cannot_endorse`; `422 too_far {distance_m}` |
| `GET /events/{id}/shelters-to-endorse?endorser_shelter_id` | — | `[{shelter_id, name, address, creator_name, distance_m, photo_count}]` |
| `POST /shelters/{id}/endorsements/remote` | `{endorser_shelter_id}` | `200 {endorsements, confirmed: bool}` |

### 8.4 Convites e membros
| Método e rota | Corpo | Resposta |
|---|---|---|
| `POST /shelters/{id}/invites` | `{kind: qr|link, role}` (admin) | `201 {code, url, expires_at}`; `422 link_volunteer_only` |
| `GET /shelters/{id}/invites` | — (admin) | ativos e quem entrou por link |
| `DELETE /invites/{code}` | — (admin) | `204` |
| `GET /invites/{code}` | público | `{shelter_name, role, confirmed, valid}` |
| `POST /invites/{code}/accept` | público `{capture_id, profile?}` | `201 {session, membership}`; `410 invite_invalid`; `409 already_member`; `403 banned\|removed_from_shelter` |
| `GET /shelters/{id}/members` | — (membro) | lista |
| `PATCH /shelters/{id}/members/{volunteer_id}` | `{role: admin}` (admin) | `200` |
| `DELETE /shelters/{id}/members/{volunteer_id}` | — (admin remove voluntário; DC remove qualquer um) | `204`; `403 admin_cannot_remove_admin`; `409 last_admin` |
| `POST /shelters/{id}/leave` | — | `204`; `409 last_admin` |
| `POST /volunteers/{volunteer_id}/ban` | `{reason}` (DC) | `200` |

### 8.5 Pessoas e cartões
| Método e rota | Corpo | Resposta |
|---|---|---|
| `POST /shelters/{id}/checkins` | `{id, capture_id? \| photo_base64? \| no_biometrics: true, data: {name?, wristband?, is_child, birth_year?, needs?}, thumbnail_base64?, legal_basis: consent\|vital_interest\|child_protection, client_created_at}` | `201 {person_id, level, is_new, history}`; `422 no_face\|capture_inconclusive\|invalid_image` |
| `GET /shelters/{id}/people` | — (membro) | pessoas com `current_shelter_id = id` (+ `stays` filtradas) |
| `GET /shelters/{id}/people/needs-selfie` | — | `assisted` no abrigo |
| `GET /people/{person_id}` | — (membro do `current_shelter_id` ou de abrigo onde a pessoa passou) | pessoa com `stays` filtradas por 2.3 |
| `PATCH /people/{person_id}` | `{data, client_edited_at}` | `200`; apaga a miniatura se `name` ou `wristband` vierem preenchidos |
| `POST /people/{person_id}/guardians` | `{guardian_id}` | `200`; `422 guardian_not_verified` |
| `GET /people/{person_id}/thumbnail` | — (4.6) | `image/jpeg`, auditado |
| `POST /shelters/{id}/exits` | `{person_id, guardian_id?, capture_id?, exception?: {reason}}` | `200`; `409 revalidation_failed` |
| `POST /shelters/{id}/cards` | `{code, person_ids: [], client_issued_at}` | `201`; `409 code_taken\|person_has_card` |
| `GET /cards/{code}` | — (membro de qualquer abrigo) | `{code, status, members: [{person_id, name, level}]}` (sem histórico) |
| `PATCH /cards/{code}` | `{add_person_ids?, remove_person_ids?}` | `200` |
| `POST /cards/{code}/cancel` | `{replaced_by?}` | `200` |

### 8.6 Entregas
| Método e rota | Corpo | Resposta |
|---|---|---|
| `POST /shelters/{id}/deliveries` | `{id, card_code, person_id, item, quantity, delivered_at, capture_id?}` | `201 {delivery_id, status, reasons}`; `428 liveness_required`; `409 revalidation_failed`; `404 card_not_found\|person_not_on_card` |
| `GET /shelters/{id}/deliveries?status=` | — (admin) | lista |
| `POST /deliveries/{id}/review` | `{decision: reviewed_ok\|irregular, note}` (admin) | `200` |

### 8.7 Resgates, buscas, matches, "Estou Seguro"
| Método e rota | Corpo | Resposta |
|---|---|---|
| `POST /rescues` | `{id, photo_base64, lat, lon, accuracy, captured_at, data?: {name?, wristband?, note?}, landing_point?, destination_shelter_id?, thumbnail_base64?}` | `201 {rescue_id, person_id, level}` |
| `PATCH /rescues/{id}` | `{landing_point?, destination_shelter_id?}` (autor) | `200` |
| `POST /searches` | `{id, photos_base64: [1..5], missing_person: {name, notes}, family: {name, phone, relationship, language}}` | `202 {message: "search_registered", usable_photos}` (igual com ou sem match) |
| `GET /matches` | — (mediadores) | fila dos abrigos que o voluntário administra + fila DC se DC |
| `GET /matches/{id}` | — | detalhes sem contato da família |
| `POST /matches/{id}/decision` | `{decision: accept\|decline, decided_by: person\|operator, note?}` | `200 {family?, whatsapp_link?}` |
| `POST /safe-check` | público `{capture_id, status: safe\|went_to_shelter, reported_shelter_id?, lat?, lon?, family_contact_allowed, language}` | `200` |

### 8.8 Painel, alertas, exportação
| Método e rota | Auth | Resposta |
|---|---|---|
| `GET /dashboard/events` | Val, DC | eventos com contagens |
| `GET /dashboard/events/{id}` | Val, DC | 3.11 |
| `GET /alerts` | admin (alertas dos seus abrigos), DC (todos) | lista |
| `POST /alerts/{id}/resolve` | admin destinatário, DC | `200` |
| `GET /shelters/{id}/export.csv` | admin de abrigo `confirmed` | CSV das pessoas e entregas do próprio abrigo, sem miniatura nem `person_id`, auditado. **PROPOSTA (a confirmar)** (Q22) |

---

## 9. Sincronização offline e conflitos

### 9.1 O que funciona offline
Criar abrigo, check-in por foto (`assisted`), check-in sem biometria, emissão e leitura de cartão, edição de dados de pessoa, saída de adulto, entrega por cartão, resgate, busca (PROPOSTA, Q45), atualização de endereço. **Não funciona offline:** login, convites, capturas com liveness (inclui entrega de alto valor revalidada e saída de criança), avais, confirmação, mediação, painel.

### 9.2 Fila
- Cada escrita vira um item em `queue` com `id` UUID, usado como `Idempotency-Key` e, quando a entidade é nova, como ID do documento.
- Drenagem: ao abrir o app, no evento `online`, logo após enfileirar e a cada 30 s enquanto houver pendentes e o app estiver aberto (iOS não tem Background Sync).
- FIFO, um item por vez. Por resultado:
  - `2xx` ou resposta idempotente: remove o item e o `Blob`; atualiza caches.
  - `401`: para a drenagem e pede login. Depois, só reenvia itens cujo `volunteer_id` é o da nova sessão; os de outro voluntário ficam `held` com "entre como <nome> para enviar".
  - `409 shelter_nearby` na criação: o item e todos os do mesmo `shelter_id` ficam `held` até o admin escolher (3.2.1).
  - `422` de imagem/rosto: `needs_action`; o app lista em "Pendências".
  - `428 liveness_required` de entrega enfileirada: não acontece (o app manda entregas offline como `liveness_skipped_offline`, 3.5.2).
  - `429`, `5xx`, `503`, rede: `attempts += 1`, `next_attempt_at = agora + min(5 s × 2^attempts, 10 min) × (0,5 a 1,5 aleatório)`; respeita `Retry-After`.
  - Outros `4xx`: `needs_action` com o código.
- Contador "N pendentes" sempre visível; aviso se houver item pendente há mais de 24 h.

### 9.3 Dependências
Itens que referenciam um abrigo criado offline vêm depois dele (FIFO). Se o abrigo estiver `held`, os dependentes também ficam.

### 9.4 Resolução de conflitos (o servidor é a fonte da verdade [dec. a])
| Caso | Regra |
|---|---|
| Mesma pessoa cadastrada em dois celulares (offline) | Mesmo token → mesmo `person_id`. O segundo check-in vira atualização: acrescenta `stay`; nível só sobe. |
| Mesma pessoa em dois abrigos ao mesmo tempo | A `stay` com `entered_at` mais recente vira `current_shelter_id`; a outra fecha com `exit_reason = checkin_elsewhere`. Entradas a menos de 1 h e abrigos a > 50 km: alerta `anomaly_distance`. |
| Mesma pessoa em dois cartões emitidos offline | O primeiro a sincronizar fica; o segundo recebe `409 person_has_card` → `needs_action` ("pessoa já tem o cartão X"). |
| Edição de dados da pessoa | Último a editar ganha, por `client_edited_at`; edição mais antiga é ignorada (`200` com o estado atual). |
| Item repetido (retentativa) | `Idempotency-Key` devolve a resposta original; nenhuma nova chamada à Unico. |
| Dois abrigos a < 100 m criados offline | 3.2.1. |
| Fusão de abrigos aceita | Para cada pessoa com `stay` na origem: troca `shelter_id` origem→destino; se já havia `stay` aberta no destino, mantém a de `entered_at` mais antiga e fecha a outra. Cartões, entregas e resgates: `shelter_id` reapontado. Membros da origem viram `volunteer` do destino (`via = merge`). Origem: `status = merged`, `merged_into = destino`. Itens da fila que chegarem depois para a origem são aplicados ao destino. |
| Fusão de eventos | 4.3. |
| Escrita para abrigo de evento encerrado | `409 event_closed` → `needs_action` (PROPOSTA, Q36). |

---

## 10. Segurança e privacidade

### 10.1 LGPD

#### 10.1.1 Base legal por operação (gravada em `audit_log.legal_basis`)
| Operação | `legal_basis` |
|---|---|
| Check-in de adulto consciente | `consent` (art. 11, I) |
| Check-in de pessoa ferida/incapacitada, resgate, miniatura | `vital_interest` (art. 11, II, "e") |
| Check-in e saída de criança | `child_protection` (art. 14, § 3º) |
| Entrega de doação e revalidação | `fraud_prevention` (art. 11, II, "g") |
| Busca de desaparecido e mediação | `vital_interest` |
| Dados de contato da família | `family_consent` (art. 7º, I) |
| "Estou Seguro" | `consent` |
| Credenciamento de voluntário | `fraud_prevention` (**PROPOSTA (a confirmar)**, Q35) |
| Painel e operação da Defesa Civil | `public_policy` (art. 11, II, "b") |

#### 10.1.2 Descarte
- Evento encerrado → apaga miniaturas imediatamente [dec. e].
- `purge_at = closed_at + PURGE_AFTER_DAYS` (PROPOSTA 30, Q9). No descarte:
  - pessoas cujos eventos em `event_ids` estão todos com `purge_at ≤ agora` (ou fundidos em um assim): apagadas;
  - cartões, entregas, resgates, buscas, matches, fusões, alertas, memberships, fotos de local e abrigos do evento: apagados;
  - evento: fica só com `name`, datas e `stats`, `status = purged`.
- "Estou Seguro" sem evento: apagado 30 dias após o registro (PROPOSTA).
- Perfis de voluntário: apagados 365 dias após `last_seen_at`, exceto banidos (PROPOSTA, Q21).
- Auditoria: TTL de 1 ano (PROPOSTA, Q21).
- Fotos de pessoas nunca ficam no servidor: só existem na memória do processo durante a chamada à Unico [dec. e]. No celular, o `Blob` é apagado após o `2xx`.

#### 10.1.3 Transparência
Texto curto em cada tela de coleta (check-in, "Estou Seguro", busca): finalidade, prazo de guarda, quem acessa, base legal, nome do controlador. Consentimento e "Estou Seguro" em PT, EN e ES [dec. h, M3]. Página `/privacy` com o texto completo.

#### 10.1.4 RIPD, região e transferência internacional
- O repositório deve conter `docs/pt_br/RIPD.md` antes de usar com dados reais (art. 38): dados tratados (ID derivado do token, miniatura, nome, necessidades, localização, cartão, contato da família), finalidades, bases (10.1.1), riscos (agressor, abrigo falso, operador mal-intencionado, vazamento), medidas (esta seção), descarte.
- Região [dec. h, A8]: o piloto fica nos EUA (`us-east1`). O RIPD registra a transferência internacional (art. 33) e documenta que cada controlador pode subir a própria instância em `southamerica-east1` (São Paulo), ao custo de sair da camada gratuita, sem mudança de código: só o inventário Ansible muda.
- Mecanismo do art. 33 e definição de controlador/operador: Q10.

### 10.2 Proteção contra abrigo falso
- Nasce `unconfirmed`: vê só o que cadastrou, não busca, não media, não aceita fusão, não exporta [dec. d].
- Tela de consentimento mostra "Abrigo não confirmado pela Defesa Civil".
- Criação exige perfil com liveness: o criador é identificável e banível.
- Aval remoto não gera avalistas (2.2). Revogação sinaliza avalistas.
- Abrigo a < 100 m de outro: bloqueado ou `flagged` para a DC.

### 10.3 Proteção contra agressor que busca a vítima
- Busca só por operador de abrigo confirmado ou DC; limite por voluntário (PROPOSTA, Q8).
- Resposta da busca idêntica com ou sem match.
- Mediador é do abrigo onde a pessoa está (ou DC), não do abrigo de quem buscou.
- Contato da família só aparece ao mediador depois de a pessoa aceitar.
- Localização nunca é enviada automaticamente.
- Leitura de cartão (`GET /cards/{code}`) devolve só nomes e nível dos membros, sem abrigo atual nem histórico.
- Busca grava auditoria com quem buscou.
- Risco residual: agressor que é admin do mesmo abrigo da vítima. Mitigação só por auditoria; registrado no RIPD.

### 10.4 Endpoints públicos e custo
- `POST /captures` público e `POST /safe-check`: 5 capturas por IP por hora (`governor`, em memória) e `PUBLIC_CAPTURES_PER_DAY` no total (contador em memória, zerado à meia-noite UTC). PROPOSTA (Q15).
- Corpo máximo de 3 MB; imagem individual ≤ 800 KB.
- Liveness na entrega só por amostragem e alto valor reduz o consumo de processos da Unico [dec. h, A1].

### 10.5 Segredos
`HUMAN_TOKEN_ACCESS_KEY`, `HMAC_KEY` e `THUMBNAIL_KEY` ficam só no vault de cada instalação (`inventory/prod/`, fora do git) e em `/etc/taba/env` na VM; `HMAC_KEY` tem backup fora da VM [dec. i, Q3]. Nunca no repositório (`.env` e `inventory/prod/` estão no `.gitignore`).

---

## 11. Idiomas da interface (i18n) [dec. f, h]

### 11.1 Regra geral
- Interface em `pt-BR` e `en`. Arquivos `pwa/src/i18n/pt-BR.json`, `en.json` e `es.json` (este só com as chaves de 11.3). Novo idioma = novo arquivo.
- Detecção: primeiro valor de `navigator.languages` que começa com `pt` → `pt-BR`; senão `en`.
- Seletor "PT | EN" fixo no topo. Escolha salva em `config.language` e, logado, em `volunteers.language` (`PATCH /me`). Ao logar, o idioma do perfil prevalece.
- Links enviados (convite, aviso à família) não carregam idioma: quem abre vê no idioma do próprio celular.
- Mensagem de WhatsApp à família: no `family.language` informado na busca (`pt-BR` ou `en`).
- Datas e números: `Intl.DateTimeFormat` e `Intl.NumberFormat`.
- Dados digitados não são traduzidos. Erros do servidor chegam como códigos estáveis e são traduzidos pelo app.

### 11.2 O que fica em inglês
Identificadores, rotas, logs, commits e PRs (seção 0) [dec. j].

### 11.3 Espanhol [dec. h, M3]
Só duas telas: a tela de consentimento (check-in) e a página `/safe`.
- **PROPOSTA (a confirmar) (Q43):** essas duas telas têm seletor próprio "PT | EN | ES". Na tela de consentimento o seletor é independente do idioma do voluntário (quem lê o consentimento é a pessoa atendida); o padrão é o idioma da interface. Em `/safe`, o padrão vem de `navigator.languages` (`es*` → `es`). Fora dessas telas, quem escolheu `es` vê `en`.

---

## 12. Questões abertas e propostas a confirmar

Resolvidas em 2026-10-21 e retiradas daqui: Q1–Q5 [dec. i], Q11 (licença AGPL-3.0), Q12 (repositório `github.com/thingsbuilder/taba`) e Q14 (domínio `taba.thingsbuilder.com`) [dec. g]. Ver seção 13.

### 12.1 Pendências do dono
- **Q13.** Primeira tela a construir. A primeira tarefa já está definida (teste da jornada no iPhone, [dec. i]); depois dela, a ordem mínima é login → criar abrigo → check-in.

### 12.2 Questões de produto e jurídicas
- **Q6.** Os termos da Unico permitem `api+humantoken` com fotos que não vieram do SDK (resgate, busca)?
- **Q7.** Plano e limite mensal de processos da Unico; quem paga.
- **Q8.** PROPOSTA: busca só por operador de abrigo confirmado ou DC; 5 fotos por busca; 10 buscas por voluntário por dia; busca feita pela família fora da v1.
- **Q9.** PROPOSTA: descarte 30 dias após o encerramento do evento.
- **Q10.** Controlador e operador; mecanismo do art. 33; validação jurídica do RIPD.
- **Q15.** PROPOSTA: 5 capturas públicas por IP por hora e 500 por dia no total.
- **Q16.** PROPOSTA: duplicidade de entrega por pessoa, não por núcleo familiar (o cartão agrupa o núcleo, mas a regra conta por pessoa); entrega offline que exigiria liveness nasce em revisão.
- **Q17.** PROPOSTA: período padrão de 7 dias por item, configurável por abrigo.
- **Q18.** PROPOSTA: revalidação do responsável falhou → saída bloqueada; admin pode registrar saída por exceção, auditada, com alerta à DC.
- **Q19.** PROPOSTA: sem notificação push na v1.
- **Q20.** PROPOSTA: revogação não afeta os abrigos que o revogado avalizou; só sinaliza quem deu aval a ele.
- **Q21.** PROPOSTA: auditoria por 1 ano; perfil de voluntário apagado 1 ano após o último acesso, exceto banidos.
- **Q22.** PROPOSTA: "ver dados de outros abrigos" = histórico de uma pessoa presente no seu abrigo; sem pesquisa livre por nome na rede; exportação só do próprio abrigo, sem `person_id`.
- **Q23.** PROPOSTA: papéis globais `validator` e `civil_defense` com poderes diferentes (2.1), concedidos por CLI.
- **Q24.** PROPOSTA: pessoa em abrigo não confirmado ou sem abrigo → fila da DC; "Estou Seguro" com consentimento prévio para a família saber só que está segura.
- **Q25.** PROPOSTA: busca ativa até o descarte do evento do abrigo de origem, comparada com cada novo cadastro.
- **Q26.** PROPOSTA: sessão de 7 dias, renovada a cada uso.
- **Q27.** A selfie no início de cada turno (ideia 4 do histórico) fica fora da v1?
- **Q28.** PROPOSTA: registro `no_biometrics` quando não há como gerar token; fica fora da busca por foto.
- **Q29.** PROPOSTA: até 2 fotos do local por abrigo, ≤ 200 KB, no próprio Firestore.
- **Q30.** PROPOSTA: "ligado ao abrigo novo" = ter ou ter tido `membership` com ele.
- **Q31.** PROPOSTA: abrigo que declarou evento distante participa da cadeia; "região ou estado" = mesma UF ou ≤ 300 km.
- **Q32.** PROPOSTA: sem backup do Firestore na v1 (exportação agendada e PITR têm custo).
- **Q33.** PROPOSTA: miniatura de resgatado ainda sem abrigo visível ao voluntário do ponto de desembarque que registrou e à DC.
- **Q34.** PROPOSTA: fotos para token reduzidas a ≤ 300 KB.
- **Q35.** PROPOSTA: base legal do credenciamento de voluntário = art. 11, II, "g".
- **Q36.** PROPOSTA: abrigo de evento encerrado não aceita novas escritas.
- **Q37.** PROPOSTA: voluntário removido só volta ao abrigo por QR presencial.
- **Q38.** PROPOSTA: código de aval presencial expira em 10 min.
- **Q39.** PROPOSTA: limiares de anomalia: 30 check-ins em 10 min; 100 resgates em 10 min (ponto de desembarque [dec. h, A11]); mesma pessoa a > 50 km em < 1 h; resgate fora do raio do evento.
- **Q40.** PROPOSTA: na fusão aceita, admins da origem viram voluntários no destino.
- **Q41.** PROPOSTA: amostragem de liveness em 5% das entregas comuns, por sorteio determinístico pelo `delivery_id`; a lista de itens de alto valor é configurada por abrigo.
- **Q42.** PROPOSTA: pessoa `assisted` só recebe doação online após captura com liveness (que a sobe para `verified`); offline, a entrega vai para revisão.
- **Q43.** PROPOSTA: seletor PT | EN | ES próprio nas duas telas com espanhol (11.3).
- **Q44.** PROPOSTA: código do cartão com 8 caracteres Crockford base32, gerado no celular; uma pessoa em no máximo um cartão ativo; o cartão vale em toda a rede.
- **Q45.** PROPOSTA: busca registrada offline vai pela fila (a revisão do especialista recomenda não fazer, ver 12.3, seção 4 item 8).

### 12.3 Da revisão do especialista — aguardando decisão
Achados da revisão do especialista em gestão de desastres (documento interno) que o dono ainda não decidiu. Não estão no corpo da spec.

| ID | Resumo | Recomendação do especialista |
|---|---|---|
| C1 | Consentimento não é livre quando recusar a selfie reduz abrigo ou doação; afeta `no_biometrics` (Q28). | Regra "recusar a selfie não tira abrigo nem doação"; `no_biometrics` como caminho de primeira classe, com doação por lista e duplicidade só no abrigo; botões de mesmo peso; taxa de recusa no painel. |
| C2 | Criança desacompanhada ou separada não tem fluxo; revalidação prova que é o mesmo adulto, não o responsável legal. | Estados com responsável / separada / desacompanhada (nomes em inglês se aceito); alerta à DC e ao Conselho Tutelar; saída só por papel de proteção; vínculo "declarado" ou "verificado com documento"; match de criança sempre à fila de proteção; confirmar com a Unico o uso em menores. |
| A2 | Duplicidade por pessoa ignora que cesta e kit de limpeza são por família (discorda de Q16). | Marcar cada item como por pessoa ou por família; duplicidade por família compara o núcleo. |
| A3 | Falta o núcleo familiar como entidade completa (hoje o cartão agrupa membros, sem chefe nem dados da família). | `families` com membros, chefe declarado e número do cartão. |
| A4 | Mediação por admin voluntário sem treino; contato da família fica no WhatsApp pessoal. | Papel `mediador` concedido por DC/validador; sem ele, fila da DC; número institucional ou orientação de apagar o contato; criança sempre na fila de proteção. |
| A5 | Busca isolada dos canais oficiais; busca some em silêncio no descarte (ajusta Q8 e Q25). | Busca também por Cruz Vermelha/assistência social; avisar quem registrou antes de apagar busca sem match; dizer na tela que não substitui o registro policial; óbitos fora do escopo. |
| A6 | Falso positivo de match não tem tratamento. | Mediador confere com a pessoa os dados da família antes de aceitar; decisão "não é a pessoa" (nome em inglês se aceito); medir a taxa no piloto. |
| A7 | Painel não responde ao que a DC reporta (desabrigados x desalojados, recortes, vagas). | Capacidade e lotação, contagem incluindo `no_biometrics` e `assisted`, recortes por faixa etária, gestante, PcD, desacompanhados, saídas por destino, CSV diário para o boletim. |
| A9 | Sem controlador não há piloto. | Termo com um município ou organização como controlador antes de qualquer dado real; a DC indica quem recebe os papéis globais. |
| A10 | Descarte de 30 dias é curto para entregas com recurso público e para buscas sem resolução (ajusta Q9). | Descarte diferenciado: biometria e miniatura em 30 dias; entrega sem biometria pelo prazo de prestação de contas do controlador; busca ativa encaminhada antes do apagamento. |
| A12 | Dependência de um fornecedor e de uma VM gratuita no pico. | Modo degradado documentado: sem Unico ou sem cota, cadastro por nome/pulseira e tokens depois; redimensionar a VM antes de evento real. |
| M1 | Rotatividade de voluntários e celular compartilhado deixam itens retidos para sempre. | Admin assume itens retidos de outro voluntário (auditado); sessão por aparelho do abrigo com troca de operador por PIN local. |
| M2 | Grupos vulneráveis e identidade (demência, lesão facial, pessoa trans, indígenas, migrantes). | Campo nome social; campos estruturados de vulnerabilidade; consentimento lido em voz alta; frase de que os dados não vão para polícia nem imigração. |
| M4 | "Necessidades" em texto livre vira dado de saúde. | Lista fechada operacional (medicação contínua, restrição alimentar, mobilidade reduzida, acompanhamento de saúde). |
| M5 | Saída de adulto sem destino. | Campo de destino da saída (lista fechada) e município; no painel. |
| M6 | Histórico de outros abrigos visível a qualquer voluntário de abrigo confirmado. | Histórico só para admin ou assistência social; voluntário vê só "já passou por outro abrigo: sim". |
| M7 (baixo) | Raio de 100 m e GPS ruim dentro de prédios geram muitas justificativas. | Aceitável; registrar no piloto quantas justificativas aparecem para calibrar o raio. |
| Seção 3 | Lacunas operacionais: capacidade e vagas do abrigo; papel de proteção; desabrigado x desalojado; animais acompanhantes; contatos do Conselho Tutelar e da DC local por evento; integração com CadÚnico fora da v1 (só CSV cruzável, nunca o ID do token); material de treinamento. | Propostas mínimas da tabela da seção 3 da revisão. |
| Seção 4 | O que não fazer na v1: "Estou Seguro" inteiro (deixar para a v2); aval remoto e cadeia de confirmação; fusão de eventos e de abrigos (DC resolve à mão no piloto); fotos do local e Nominatim; busca offline pela fila (Q45). | Cortar da ordem de construção do piloto, sem mudar o que o sistema é. |
| Seção 5 | Posições sobre propostas: ajusta Q8 (incluir papel de proteção; limite por papel), Q9, Q17 (período por item, inclusive "uma vez por evento"), Q15 (limite por aparelho + captcha, não por IP), Q16 ("aprovar todos do dia"), Q25; discorda de Q18 (exceção só por papel de proteção ou DC); ressalvas em Q19 (aviso fora do app para criança) e Q24 (fila da DC com pessoas nomeadas e prazo); concorda com as demais. | Ver tabela da seção 5 da revisão. |
| Seção 6 | Plano de validação: bancada com pessoas reais, simulado com a DC, abrigo de rotina; métricas e critério para desistir de uma funcionalidade. | Seguir as etapas e métricas da seção 6 da revisão antes de evento real. |

---

## 13. Registro de decisões

### 13.1 Decisões do dono incorporadas nesta versão
| Decisão | Onde |
|---|---|
| Q1 [dec. i]: check-in offline nasce `assisted` e sobe na primeira captura com liveness | 3.4, 4.1 |
| Q2 [dec. i]: fusão de abrigos é pedido aceito por admin do destino | 3.2.1, 9.4 |
| Q3 [dec. i]: o banco guarda o HMAC do token, nunca o token puro; `HMAC_KEY` no vault, com backup | 6.3.2, 10.5 |
| Q4 [dec. i]: liveness pela jornada; primeira tarefa = testar a jornada na PWA instalada no iPhone; plano B SDK web | 5.1 |
| Q5 [dec. i]: saída acima de 1 GB/mês é custo conhecido (~US$ 0,12/GB) | 6.1.4, 6.4 |
| Q11: licença AGPL-3.0 | `LICENSE` |
| Q12: repositório público `github.com/thingsbuilder/taba` | — |
| Q14 [dec. g]: domínio `taba.thingsbuilder.com`; projeto Taba; região `us-east1` | 6, 6.4 |
| C3 [dec. h]: "preciso de resgate" vira orientação 193/199 sem gravar alerta | 3.8 |
| A1 [dec. h]: conferência no check-in; entrega por cartão; liveness só em alto valor e por amostragem | 3.5, 4.7 |
| A11 [dec. h]: resgate registrado no ponto de desembarque; limiar de anomalia maior | 3.6, 4.10 |
| A8 [dec. h]: EUA no piloto; região por instalação; RIPD documenta `southamerica-east1` | 6, 10.1.4 |
| M3 [dec. h]: consentimento e "Estou Seguro" também em espanhol | 11.3 |
| [dec. j]: implementação, logs, commits e PRs em inglês | 0 |

### 13.2 Decisões técnicas
| Decisão | Motivo |
|---|---|
| Revalidação por comparação de IDs derivados | Mesmo resultado e custo do flow `+revalidation`, sem guardar o token puro. Rejeitado `humantoken+revalidation`: exigiria o token puro em `references`. |
| Lock global em memória para criação de abrigo | Uma instância; resolve a corrida dos 100 m. Rejeitado: transação com consulta geográfica (9 consultas) e documento-trava por célula (complexidade sem necessidade). |
| Tarefa horária dentro do processo | Uma unidade systemd a menos. Rejeitado systemd timer/cron. |
| Idempotência genérica por `Idempotency-Key` | A fila só reenvia requisições HTTP; uma regra para todas as rotas e nenhuma chamada repetida (paga) à Unico. Rejeitado endpoint `/sync` em lote. |
| IDs gerados no cliente para entidades criadas offline | Referência entre itens da fila antes da sincronização e deduplicação natural. |
| Sorteio de liveness determinístico pelo `delivery_id` | A retentativa da mesma entrega não muda o resultado do sorteio. |
| Código do cartão curto e digitável | Funciona com pulseira improvisada e sem câmera boa. |
| Fotos do local no Firestore (bytes) | Evita Cloud Storage para ~2 fotos pequenas por abrigo. |
| Busca responde igual com ou sem match | Até "não encontrado" é informação para um agressor. |
| Geohash com 9 células + haversine | Exigido pela dec. g; consultas de intervalo simples. |
| Preact + `idb` + `vite-plugin-pwa`; `BarcodeDetector` com `jsQR` de reserva | PWA pequena; Safari no iOS não tem `BarcodeDetector`. Rejeitado React (peso). |
| Mediação por mensagem `wa.me` | Sem serviço pago de SMS/e-mail. |
| Nominatim chamado pelo celular | Permitido pela dec. g; evita saída na VM. |
| Infra só pelos playbooks existentes | `provision.yml`/`configure.yml` já criam VM, Firestore e Caddy; o deploy só acrescenta tarefas (serviço, swap, Caddy com proxy, variáveis). |

---

## 14. Estrutura de arquivos do repositório

```
api/                          Rust (cargo), binário `taba`
  Cargo.toml
  src/main.rs config.rs auth.rs unico.rs geo.rs shelters.rs events.rs members.rs
      people.rs cards.rs deliveries.rs rescues.rs searches.rs safe_check.rs
      dashboard.rs jobs.rs audit.rs idempotency.rs
  tests/                      integração com Unico falso e emulador do Firestore
pwa/                          Vite + TypeScript + Preact
  src/main.tsx routes.ts api.ts queue.ts db.ts camera.ts image.ts geo.ts nominatim.ts qr.ts
  src/screens/*.tsx
  src/i18n/pt-BR.json en.json es.json
infra/                        já existe
  README.md
  ansible/playbooks/provision.yml configure.yml   (configure ganha: serviço, swap, deploy)
  ansible/templates/Caddyfile.j2 env.j2           (Caddyfile ganha reverse_proxy; env.j2 ganha variáveis)
  ansible/templates/taba.service.j2               (novo)
  ansible/inventory/example/                      (no git)
  firestore/firestore.indexes.json                (novo)
.env.example                  todas as variáveis da 6.2.2, sem valores
docs/pt_br/spec/spec.md       este arquivo (versão em inglês: docs/en/spec/spec.md)
docs/pt_br/RIPD.md            antes de dados reais
```

Commits e pull requests em inglês [dec. j].

---

## 15. Critérios de aceite

Testes automatizados usam um servidor Unico falso (`UNICO_URL`) e o emulador do Firestore. Nenhum teste automatizado chama a Unico real.

**Unico**
1. `extract_token` devolve `None` para `value` ausente, `""` e `"  "`; nenhum desses cria pessoa ou voluntário.
2. `429` com "monthly process limit" não é repetido e gera alerta `unico_limit`; `429` sem essa mensagem é repetido até 4 vezes com espera crescente e depois vira `503 unico_busy` com `Retry-After`.
3. Imagem > 800 KB → `413 image_too_large` sem chamar a Unico.
4. Captura só é consumida uma vez e só pelo `device_id` que a criou.
5. (Manual, primeira tarefa) Jornada aberta a partir da PWA instalada no iPhone volta pelo `callbackUri` e gera sessão.

**Abrigo e evento**
6. `accuracy` 101 m → `422 insufficient_accuracy`.
7. Segundo abrigo a 80 m de um `open` → `409 shelter_nearby` com primeiro nome do criador e sem telefone se `show_phone = false`; com `other_site_reason` → criado `flagged` e alerta `shelter_flagged`.
8. Abrigo a 40 km de abrigo de evento ativo entra nele; a 60 km cria evento novo; a ≤ 50 km de abrigos de dois eventos funde os eventos e o mais antigo sobrevive.
9. Evento sem escrita há 30 dias vira `closed` na tarefa horária, com `purge_at`; miniaturas das suas pessoas são apagadas.
10. Duas criações simultâneas a 50 m: só uma é criada sem conflito.
11. Fusão pedida por abrigo `unconfirmed` só se completa quando um admin do destino aceita; antes disso o destino não vê dados da origem.

**Confirmação**
12. Abrigo `confirmed` só por aval remoto → `403 cannot_endorse`; após aval presencial, consegue avalizar.
13. Aval presencial com celulares a 150 m → `422 too_far`.
14. Revogação volta o abrigo a `unconfirmed` e sinaliza os avalistas.
15. Abrigo `unconfirmed`: `POST /searches` → `403`; exportação → `403`; histórico do check-in sem `stays` de outros abrigos.

**Convites e membros**
16. Link com `role = admin` → `422 link_volunteer_only`.
17. QR usado duas vezes → segunda `410`; QR com 31 min → `410`.
18. Admin removendo admin → `403`; último admin saindo → `409 last_admin`.
19. Voluntário banido não cria sessão (`403 banned`) nem aceita convite.

**Pessoas, cartões, entregas, resgates**
20. Mesmo token em dois check-ins → um documento em `people`, duas `stays`, uma aberta.
21. Check-in offline gera `assisted`; captura com liveness posterior da mesma pessoa → mesmo documento, `verified`.
22. Resgate seguido de check-in com liveness da mesma pessoa → mesmo documento, `verified`, resgate no histórico.
23. Entrega comum por cartão, não sorteada, para pessoa `verified` → `201` com `verification = card`, sem chamada à Unico.
24. Entrega de item `high_value` sem `capture_id` → `428 liveness_required`; com captura de outra pessoa → `409 revalidation_failed` e nada gravado.
25. Mesma entrega reenviada (mesmo `delivery_id`) tem o mesmo resultado de sorteio.
26. Entrega do mesmo item à mesma pessoa dentro do período em outro abrigo de outro evento ativo → `201` com `status = in_review`, motivo `duplicate_in_period`.
27. Entrega offline de item `high_value` → `in_review`, motivo `liveness_skipped_offline`.
28. Pessoa em dois cartões ativos → o segundo recebe `409 person_has_card`.
29. `GET /cards/{code}` não devolve abrigo atual nem histórico.
30. Saída de criança com captura de outra pessoa → `409 revalidation_failed`, `stay` aberta.
31. Preencher `name` de pessoa com miniatura apaga `thumbnail_encrypted` na mesma escrita.
32. `GET /people/{id}/thumbnail` por voluntário de outro abrigo → `403`; acesso permitido grava auditoria.
33. 50 resgates do mesmo voluntário em 10 min não geram `anomaly_volume`; 50 check-ins geram.

**Busca, mediação, "Estou Seguro"**
34. `POST /searches` devolve corpo idêntico com e sem match.
35. Match de pessoa em abrigo `confirmed` aparece só para os admins desse abrigo; o contato da família só vem com `decision = accept`.
36. Check-in posterior de pessoa procurada cria match; repetir não cria segundo match.
37. "Estou Seguro" sem consentimento prévio gera match `declined_in_advance` e nenhum aviso.
38. A página `/safe` mostra os botões `tel:193` e `tel:199` e não existe rota nem coleção que grave pedido de resgate.

**Offline e sincronização**
39. Com a rede desligada: criar abrigo, 3 check-ins por foto, 1 cartão, 2 entregas e 1 resgate; religar: tudo sincroniza em ordem, os `Blob`s são apagados e o contador volta a zero.
40. Reenviar o mesmo item da fila não gera segunda chamada à Unico.
41. Item com `401` não é reenviado com a sessão de outro voluntário.

**Idiomas**
42. Celular em `pt-PT` abre em pt-BR; em `es` abre em inglês, exceto a tela de consentimento e `/safe`, que abrem em espanhol; escolha manual persiste após recarregar e, logado, em outro aparelho.

**Segurança e implementação**
43. Nenhuma resposta da API contém o token Unico puro; nenhum documento do Firestore contém o token puro nem foto de pessoa (só miniatura cifrada).
44. A chave da Unico, `HMAC_KEY` e `THUMBNAIL_KEY` não aparecem em nenhum arquivo do repositório nem da PWA compilada; o repositório não contém ID de projeto GCP nem IP.
45. Rotas, coleções, campos, enums, variáveis de ambiente e mensagens de log estão em inglês (revisão de código).
