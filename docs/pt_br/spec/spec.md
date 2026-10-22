# Especificação técnica — Taba (Human Token para abrigos e resgate em desastres)

> Versão 3.1 da spec, 2026-10-21. Fontes: as decisões do dono (itens a–l, registradas na seção 13), `docs/pt_br/base legal LGPD.md`, `infra/README.md`, a revisão de um especialista em gestão de desastres e os testes com a API real (estes dois últimos são documentos internos, fora do repositório) e a documentação pública da Unico Human Token.
>
> Convenções deste documento:
> - **Decisão do dono (a–l):** requisito. Citado como "[dec. a]" etc. Achados da revisão aceitos pelo dono são citados com o ID do achado, por exemplo "[dec. h, A1]" ou "[dec. k, C1]".
> - **PROPOSTA (a confirmar):** solução minha para algo que as decisões não cobrem. Toda proposta em aberto aparece na seção 12.
> - Sem marcação: escolha técnica de implementação, com a justificativa no registro de decisões (seção 13).
> - **Fase posterior:** desenho mantido na seção 16, fora do piloto [dec. k].
> - Leitura técnica da LGPD, não é parecer jurídico.
> - O texto é em português; **todo identificador é em inglês** (seção 0).

## Sumário

0. Convenções de nomes e idioma da implementação
1. Visão, objetivos e fora de escopo
2. Atores, papéis e matriz de permissões
3. Funcionalidades e fluxos
4. Regras de negócio
5. Integração com a Unico e modo degradado
6. Arquitetura e implantação
7. Modelo de dados (Firestore)
8. API REST
9. Sincronização offline e conflitos
10. Segurança e privacidade
11. Idiomas da interface (i18n)
12. Questões abertas
13. Registro de decisões
14. Estrutura de arquivos do repositório
15. Critérios de aceite
16. Fases posteriores ao piloto

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
| papéis globais: validador, Defesa Civil, mediador, proteção | `validator`, `civil_defense`, `mediator`, `protection` |
| convite | `invite` / `invites` |
| aval presencial | `endorsement` (`in_person`) / `endorsement_codes` |
| sessão | `session` / `sessions` |
| captura (jornada Unico) | `capture` / `captures` |
| pessoa atendida | `person` / `people` |
| nível: verificado, assistido, sem biometria | `verified`, `assisted`, `no_biometrics` |
| recusou a selfie | `biometrics_declined` |
| nome social | `social_name` |
| passagem por abrigo | `stay` / `stays` |
| destino da saída | `exit_destination` |
| núcleo familiar | `household` / `households` |
| responsável por criança; vínculo declarado / verificado com documento | `guardian` / `guardians`; `declared` / `document_checked` |
| situação da criança: com responsável, separada, desacompanhada | `child_status`: `with_guardian`, `separated`, `unaccompanied` |
| pulseira (número improvisado) | `wristband` |
| cartão/pulseira com QR do núcleo familiar | `card` / `cards` |
| doação (entrega) | `delivery` / `deliveries` |
| item de doação; por pessoa / por família | `item`; `per_person` / `per_household` |
| resgate | `rescue` / `rescues` |
| busca de desaparecido | `search` / `searches` |
| match; "não é a pessoa" | `match` / `matches`; `not_the_person` |
| alerta | `alert` / `alerts` |
| contatos de referência do evento | `reference_contacts` |
| capacidade do abrigo | `capacity` |
| modo degradado | `degraded` / documento `system/status` |
| auditoria | `audit_log` |
| miniatura | `thumbnail` |
| não confirmado / confirmado | `unconfirmed` / `confirmed` |
| aberto / encerrado (abrigo) | `open` / `closed` |
| ativo / encerrado / descartado (evento) | `active` / `closed` / `purged` |
| descarte | `purge` |
| sinalizado | `flagged` |
| em revisão | `in_review` |
| painel | `dashboard` |
| idempotência | `idempotency` |
| "Estou Seguro", fusão, aval remoto (fase posterior) | `safe_check`, `merge`, `remote` |

---

## 1. Visão, objetivos e fora de escopo

### 1.1 Visão
Sistema open source para abrigos e equipes de resgate em desastres (referência: enchentes do RS, 2024). Reconhece pessoas pelo rosto, sem documento, usando o Human Token da Unico: a mesma pessoa sempre recebe o mesmo token. O token não diz quem a pessoa é, só que é a mesma de antes. O sistema **não valida identidade** e não deve ser apresentado assim.

A v1 é um **piloto**, sem controlador definido [dec. k, Q10]. O alvo é a fase de abrigamento (do dia 3 até meses), em abrigos de 50 a 500 pessoas.

### 1.2 Objetivos do piloto
1. Cadastro único de pessoas atendidas entre abrigos, com histórico que acompanha a pessoa.
2. **Recusar a selfie não tira abrigo nem doação** [dec. k, C1].
3. Núcleo familiar como entidade, com cartão/pulseira com QR para a entrega do dia a dia [dec. h, A1; dec. k, A3].
4. Doação por pessoa ou por família, conforme o item, com sinalização (sem bloqueio) de retirada repetida [dec. k, A2].
5. Proteção de criança desacompanhada ou separada [dec. k, C2].
6. Registro de resgatados no ponto de desembarque ou triagem [dec. h, A11].
7. Credenciamento de voluntários com liveness.
8. Busca de desaparecidos por foto, mediada, sem nunca devolver a localização a quem buscou, ligada aos canais oficiais [dec. k, A5].
9. Revalidação na saída de criança e nas entregas de alto valor ou sorteadas [dec. h, A1].
10. Painel no formato do que a Defesa Civil reporta [dec. k, A7].
11. Funcionar sem internet e sem a Unico (modo degradado) [dec. k, A12, Q7].
12. Custo dentro da camada gratuita do GCP, menos o IPv4 externo e o tráfego de saída de um evento real [dec. g, i].

### 1.3 Fora de escopo da v1
- App nativo [dec. a]; integração com S2iD e Cemaden [dec. c].
- Integração com cadastro oficial/CadÚnico, prontuário de saúde e interoperabilidade com a Cruz Vermelha (RFL) [dec. k]. A v1 só oferece CSV (8.8).
- Óbitos e identificação de corpos (atribuição de IML e polícia científica) [dec. k, A5].
- Contagem de desalojados (em casa de parentes ou amigos): o Taba conta só desabrigados, e o painel diz isso [dec. k, A7].
- Selfie no início de cada turno: vale o login com sessão [dec. k, Q27].
- Triagem de benefício emergencial, cadastro de danos, horas e certificado de voluntário.
- Notificação push [dec. k, Q19]: avisos aparecem na tela "Avisos"; alerta de criança mostra o telefone de referência (4.9).
- Busca feita pela própria família, sem operador [dec. k, Q8].
- Espanhol fora da tela de consentimento [dec. h, M3].
- Backoffice web para `validator` e `civil_defense`: concedidos por linha de comando na VM [dec. k, Q23].
- Adiados para depois do piloto (seção 16) [dec. k]: "Estou Seguro"; aval remoto e cadeia de confirmação; fusão de abrigos e de eventos; fotos do local.

---

## 2. Atores, papéis e matriz de permissões

### 2.1 Atores

| Ator | Como existe no sistema | Autenticação |
|---|---|---|
| **Pessoa atendida** | Documento em `people`, membro de um `household`. Não usa o app. | Nenhuma; reconhecida pelo rosto, pelo cartão ou pelo nome/pulseira. |
| **Família que procura** | Dados de contato dentro de uma `search`. Não usa o app. | Nenhuma. |
| **Voluntário** | `volunteers` + `memberships` com `role = volunteer`. Qualquer pessoa que fez Human Token com liveness tem perfil [dec. b]. Inclui o operador do ponto de desembarque [dec. h, A11]. | Sessão após jornada com liveness; troca de operador no aparelho por PIN local (3.1.1). |
| **Admin de abrigo** | `memberships` com `role = admin`. O criador do abrigo é o admin inicial [dec. a]. É o mediador padrão dos matches do próprio abrigo [dec. k, A4]. | Igual ao voluntário. |
| **Validador** | `global_roles` contém `validator` (Cruz Vermelha, prefeituras, Defesa Civil) [dec. d]. | Igual ao voluntário. |
| **Defesa Civil** | `global_roles` contém `civil_defense`. | Igual ao voluntário. |
| **Mediador** | `global_roles` contém `mediator` (assistência social, Cruz Vermelha), dado por DC ou validador [dec. k, A4]. | Igual ao voluntário. |
| **Proteção** | `global_roles` contém `protection` (Conselho Tutelar, assistência social, Cruz Vermelha) [dec. k, C2]. | Igual ao voluntário. |
| **Operador da instância** | Quem administra a VM. Não é papel no app. | SSH na VM. |

**Papéis globais [dec. k, Q23, A4]:**
- `civil_defense`: poderes de `validator` e mais: banir da rede, remover admin, ver o painel completo, atender a fila central e a de proteção, registrar saída de criança por exceção.
- `validator`: confirma e revoga abrigos; vê o painel (só leitura).
- `mediator`: atende a fila central e os matches de qualquer abrigo confirmado; registra busca.
- `protection`: atende a fila de proteção (matches de criança, crianças separadas ou desacompanhadas); registra saída de criança separada ou desacompanhada e saída por exceção; registra busca.
- `validator` e `civil_defense` são concedidos e retirados pelo operador da instância com `taba role grant|revoke <volunteer_id> <role>`, com auditoria [dec. k, Q23].
- `mediator` é concedido e retirado por DC ou validador no app (`POST /v1/volunteers/{id}/roles`) [dec. k, A4]. `protection` também é concedido e retirado por DC ou validador no app [dec. l, Q48].

### 2.2 Níveis de confiança do abrigo [dec. d]

No piloto só existem confirmação por validador e por aval presencial [dec. k]. Aval remoto: seção 16.2.

| `confirmation` / `confirmation_type` | Como chega | `can_endorse` |
|---|---|---|
| `unconfirmed` | Criação; revogação | `false` |
| `confirmed` / `validator` | Validador confirma | `true` |
| `confirmed` / `in_person` | Admin de abrigo com `can_endorse`, celulares a ≤ 100 m | `true` |

### 2.3 Matriz de permissões

Legenda: V = voluntário do abrigo; A = admin do abrigo; NC = abrigo `unconfirmed`; C = abrigo `confirmed`; Val = `validator`; DC = `civil_defense`; Med = `mediator`; Prot = `protection`. "Próprio" = do abrigo em que o ator é membro ativo.

| Operação | V (NC) | V (C) | A (NC) | A (C) | Val | DC | Med | Prot |
|---|---|---|---|---|---|---|---|---|
| Criar abrigo (qualquer perfil com liveness) | sim | sim | sim | sim | sim | sim | sim | sim |
| Check-in, núcleo familiar, cartão, entrega, saída de adulto no próprio abrigo | sim | sim | sim | sim | — | — | — | — |
| Ver os próprios cadastros | sim | sim | sim | sim | — | — | — | — |
| Ver histórico de outros abrigos de pessoa presente no próprio abrigo [dec. k, M6] | não | só "já passou por outro abrigo: sim/não" | não | sim | — | — | sim, ao mediar | sim, ao atender |
| Saída de criança `with_guardian` com vínculo `document_checked` | sim | sim | sim | sim | — | sim | — | sim |
| Saída de criança `separated`/`unaccompanied`, ou com vínculo só `declared` [dec. k, C2] | não | não | não | não | — | sim | — | sim |
| Saída de criança por exceção [dec. k, Q18, C2] | não | não | não | não | — | sim | — | sim |
| Registrar resgate (ponto de desembarque) | sim | sim | sim | sim | — | sim | — | — |
| Registrar busca por foto [dec. k, A5] | não | sim | não | sim | — | sim | sim | sim |
| Mediar match de pessoa no próprio abrigo [dec. k, A4] | não | não | não | sim | — | sim | sim | — |
| Atender fila central (abrigo NC ou sem abrigo) | não | não | não | não | — | sim | sim | — |
| Atender fila de proteção (criança) [dec. k, C2] | não | não | não | não | — | sim | — | sim |
| Ver miniatura de pessoa sem identificação no próprio abrigo | sim | sim | sim | sim | — | ao atender | ao mediar | ao atender |
| Aprovar entrega de alto valor ou sorteada sem selfie [dec. l, Q46] | não | não | sim | sim | — | — | — | — |
| Revisar entrega sinalizada; configurar itens | não | não | sim | sim | — | — | — | — |
| Definir capacidade do abrigo | não | não | sim | sim | — | sim | — | — |
| Convidar voluntário (QR ou link); promover a admin; remover voluntário | não | não | sim | sim | — | remover | — | — |
| Remover admin; banir da rede | não | não | não | não | — | sim | — | — |
| Exportar dados do próprio abrigo | não | não | não | sim | — | — | — | — |
| Aval presencial | não | não | não | sim, se `can_endorse` | — | — | — | — |
| Confirmar ou revogar abrigo | não | não | não | não | sim | sim | — | — |
| Conceder/retirar `mediator` e `protection` [dec. l, Q48] | não | não | não | não | sim | sim | — | — |
| Contatos de referência do evento | não | não | não | não | sim | sim | — | — |
| Painel da rede e CSV diário | não | não | não | não | leitura | sim | — | — |
| Renomear evento | não | não | sim | sim | — | sim | — | — |
| Encerrar o próprio abrigo | não | não | sim | sim | — | sim | — | — |

Regras de membros [dec. b]: admin não remove admin; o abrigo sempre tem pelo menos um admin ativo; o último admin só sai depois de promover outro. Um admin pode sair se não for o último.

---

## 3. Funcionalidades e fluxos

"Captura com liveness" = o app pede uma captura ao servidor, abre o link da jornada da Unico (flow `humantoken`), a pessoa faz a selfie, o app volta e o servidor obtém o token (5.2). Exige internet e a Unico disponível [dec. i, Q4].

"Foto pela câmera do app" = `getUserMedia` + quadro capturado em `canvas`, sem acesso à galeria. Gera nível `assisted`.

### 3.0 Pré-requisitos do aparelho
1. Primeiro acesso: o app gera `device_id` (UUID v4) e guarda no IndexedDB.
2. No iPhone, a instalação na tela de início é **opcional** (decisão do dono na fase 2, que substitui o bloqueio de [dec. a]). O app detecta `navigator.standalone !== true` em iOS e **não bloqueia** nada: mostra um convite curto para instalar (instalar protege os cadastros contra a limpeza do navegador; Compartilhar > Adicionar à Tela de Início), que o usuário dispensa por 7 dias. Os links de convite (3.3) abrem direto no navegador. A fase 6 revê se a instalação volta a ser exigida para o cadastro offline, porque o Safari fora da tela de início apaga o IndexedDB após cerca de 7 dias sem uso.
3. Ao abrir, o app chama `navigator.storage.persist()` [dec. a]. O app não mostra aviso sobre a resposta (decisão do dono na fase 2). O "Agora não" do convite de instalação (3.0.2) o esconde por 7 dias.
4. O app consulta `GET /v1/status` ao abrir e a cada sincronização; em modo degradado mostra a faixa de 5.7.

### 3.1 Credenciamento e login de voluntário
1. Tela "Entrar". O app chama `POST /v1/captures {purpose: "login"}` e recebe `{capture_id, link}`.
2. O app grava `capture_id` no IndexedDB e navega para `link`.
3. A jornada redireciona para `https://<domain>/return?capture=<id>`. O app chama `POST /v1/sessions {capture_id}`.
4. Perfil existe: recebe a sessão. Não existe: `422 profile_required`; o app mostra o formulário (nome, telefone opcional, "mostrar meu telefone a quem tentar criar abrigo perto do meu" [dec. a], idioma) e reenvia `POST /v1/sessions {capture_id, profile}`.
5. Perfil banido: `403 banned`.
6. Offline ou em modo degradado: "Entrar" fica desabilitado com a mensagem correspondente. Quem já tem sessão no aparelho continua trabalhando (3.1.1).
7. Sessão: 7 dias, renovada a cada uso [dec. k, Q26].

#### 3.1.1 Aparelho compartilhado e troca de operador [dec. k, M1]
- O aparelho guarda as sessões de todos os voluntários que já entraram nele online (`config.sessions`).
- Ao entrar online pela primeira vez no aparelho, o voluntário cria um PIN local de 6 dígitos. O app guarda só `PBKDF2-SHA256(PIN, salt, 200 000 iterações)`.
- "Trocar operador" lista os voluntários com sessão válida no aparelho; a troca exige o PIN e funciona offline. Cinco PINs errados seguidos apagam a sessão daquele voluntário no aparelho.
- O operador ativo é quem assina as escritas novas (`volunteer_id` do item da fila).

### 3.2 Criação de abrigo [dec. a, b, c]
1. Pré-condição: sessão válida (qualquer voluntário).
2. O app lê `navigator.geolocation.getCurrentPosition({enableHighAccuracy: true, timeout: 30000, maximumAge: 0})` e grava `lat`, `lon`, `accuracy`.
3. `accuracy > 100 m`: o app pede para ir a um lugar aberto e tentar de novo.
4. Online: o app chama o Nominatim (reverse) direto do celular e preenche o endereço [dec. k]. Offline: guarda as coordenadas com `address.pending = true` e busca depois (6.1.6).
5. O operador digita o nome ou apelido e, opcionalmente, a capacidade (número de pessoas) [dec. k, A7].
6. O app gera `shelter_id` (UUID v4) e envia `POST /v1/shelters` (direto ou pela fila).
7. Servidor, sob o lock global de criação (6.2.4):
   1. Valida `accuracy ≤ 100`.
   2. Procura abrigos `open` a ≤ `SHELTER_RADIUS_M`. Se achar e o pedido não trouxer `other_site_reason`: `409 shelter_nearby` com `{shelter_id, name, distance_m, contact: {first_name, phone?}}`. `phone` só se o criador marcou `show_phone`.
   3. Com `other_site_reason`: cria com `flagged = true` e alerta `shelter_flagged` para a DC. O painel conta essas justificativas para calibrar o raio [dec. k, M7].
   4. Atribui evento (4.3).
   5. Cria o abrigo `unconfirmed`, a `membership` do criador como `admin` e grava auditoria.
8. Na resposta `409`, o app mostra "Já existe o abrigo X; procure <primeiro nome>" e a saída "É outro local? Explique." (reenvia com `other_site_reason`).

#### 3.2.1 Conflito offline no piloto [dec. k]
1. Dois abrigos criados sem sinal a < 100 m: o segundo a sincronizar recebe o `409`; os itens da fila daquele abrigo ficam `held` (9.3).
2. A única saída no piloto é "Explique". O app sugere: "Se for o mesmo local, diga isso; a Defesa Civil resolve." O abrigo nasce `flagged`, os itens seguem, e a DC resolve à mão (por exemplo, encerrando um dos dois).
3. Fusão de abrigos por pedido (decisão Q2): seção 16.3.

### 3.3 Convite de voluntários [dec. b]
1. Admin toca "Convidar voluntário", escolhe nível (`volunteer` padrão, ou `admin`) e forma:
   - **QR presencial** (`kind = qr`): uso único, expira em 30 min, nível `admin` ou `volunteer`.
   - **Link remoto** (`kind = link`): uso único, expira em 24 h, **só `volunteer`**.
2. `POST /v1/shelters/{id}/invites` → `{code, url, expires_at}`. QR e link carregam só `https://<domain>/invite/<code>`. Exige internet.
3. O convidado abre a URL; o app mostra `GET /v1/invites/{code}` (nome do abrigo, nível, se é confirmado).
4. O convidado sempre faz captura com liveness (`purpose: "invite"`). Quem já tem perfil só faz essa captura, que é a revalidação [dec. b]; quem não tem preenche o perfil.
5. `POST /v1/invites/{code}/accept {capture_id, profile?}` → cria a `membership` e devolve a sessão.
6. Admin vê os convites ativos e cancela (`DELETE /v1/invites/{code}`). Quem convidou vê quem entrou pelo link e pode remover.
7. Recusas: convite expirado, usado ou cancelado (`410 invite_invalid`); perfil banido (`403 banned`); já é membro ativo (`409 already_member`); removido daquele abrigo antes e convidado por link (`403 removed_from_shelter`) — só volta por QR presencial [dec. k, Q37].

### 3.4 Check-in no abrigo
O check-in é o momento em que a pessoa é conferida [dec. h, A1].

**Tela de consentimento** [dec. k, C1, M2; dec. h, M3]:
- Mostra finalidade, prazo de guarda, quem acessa, o responsável pelo piloto (10.1.3) e, se o abrigo é `unconfirmed`, "Abrigo não confirmado pela Defesa Civil" [dec. d].
- Diz que **os dados não são compartilhados com polícia nem com imigração** e que **recusar a selfie não tira abrigo nem doação**.
- Tem versão curta para ler em voz alta e seletor próprio PT | EN | ES (11.3).
- Dois botões com o mesmo peso visual: "Fazer a selfie" e "Prefiro não fazer".
- O voluntário marca a base legal: `consent` (art. 11, I) ou `vital_interest` (art. 11, II, "e") se a pessoa estiver ferida ou incapacitada.

**Dados do cadastro** (`data`), todos opcionais:
- `name`, `social_name` [dec. k, M2], `wristband`, `birth_year`;
- `needs`: lista fechada `{continuous_medication, dietary_restriction, reduced_mobility, health_follow_up}` (sim/não). A tela orienta a não escrever diagnóstico [dec. k, M4];
- `vulnerabilities`: `{pregnant, lactating, disability: [mobility|visual|hearing|intellectual|psychosocial|other], indigenous_people?: texto, migrant_language?: texto}` [dec. k, M2];
- criança: ver 3.4.1.

**Caminho A — selfie com liveness (online, Unico disponível):**
1. Captura com liveness (`purpose: "checkin"`, `shelter_id`).
2. `POST /v1/shelters/{id}/checkins {id, capture_id, data, legal_basis}`.
3. Servidor, em transação sobre `people/{person_id}`:
   - não existe: cria com `level = verified`;
   - existe com `level = assisted`: sobe para `verified` e mantém o histórico;
   - fecha a `stay` aberta em outro abrigo (`left_at = agora`, `exit_reason = checkin_elsewhere`) e abre a `stay` no abrigo atual;
   - procura buscas ativas com esse `person_id` (4.8) e cria matches.
4. Resposta: `{person_id, level, is_new, history}`. `history` segue a regra de visibilidade de 4.11 [dec. k, M6].

**Caminho B — foto pela câmera do app** (ferido, inconsciente, criança pequena, offline, ou liveness falhou):
1. O app reduz a foto (6.1.4), guarda o `Blob` cifrado no IndexedDB (6.1.7) e enfileira `POST /v1/shelters/{id}/checkins {id, photo_base64, data, legal_basis}`.
2. Enquanto não sincroniza, a pessoa aparece na lista local como "pendente"; núcleo familiar e cartão já podem ser emitidos.
3. No servidor, a foto vai ao flow `api+humantoken` → `level = assisted`. O servidor descarta a foto após a chamada. Ao receber `2xx`, o app apaga o `Blob` [dec. a, e].
4. A pessoa sobe para `verified` se fizer captura com liveness depois; a selfie é **oferecida, não exigida** [dec. k, Q42 rejeitada, C1]. A lista "Pendentes de selfie" do abrigo mostra quem está `assisted`.
5. Erro de rosto (`20532`, inconclusivo): o item fica `needs_action`; o voluntário tenta outra foto ou segue pelo caminho C.

**Caminho C — sem biometria** [dec. k, C1, Q28]:
1. Quando a pessoa recusa a selfie (`biometrics_declined = true`), quando não há rosto utilizável ou em modo degradado, o voluntário registra a pessoa por nome e/ou pulseira: `POST /v1/shelters/{id}/checkins {id, no_biometrics: true, data, legal_basis}`. `person_id` = UUID do cliente, `level = no_biometrics`.
2. É um caminho de primeira classe: abrigo, núcleo familiar, cartão e doação iguais aos demais. Não entra na busca por foto (não há token). A duplicidade de doação é verificada só pelo próprio `person_id`/`household_id` (4.7).
3. Se a pessoa aceitar a selfie depois, ou se a foto ficou na fila em modo degradado: `PUT /v1/people/{person_id}/biometrics {capture_id | photo_base64}` (4.12).
4. O painel mostra a taxa de recusa por abrigo (`biometrics_declined`) [dec. k, C1].

**Pessoa sem identificação** [dec. e]: se o check-in ou o resgate chegar sem `name` e sem `wristband`, o app envia também `thumbnail_base64` e o servidor guarda cifrada (4.6).

**Núcleo familiar e cartão** (3.5.1): ao fim do check-in, o app oferece "Novo núcleo familiar" ou "Juntar a um núcleo existente" (lendo o cartão ou escolhendo na lista).

#### 3.4.1 Criança [dec. k, C2]
1. `data.is_child = true` e `data.child_status`: `with_guardian` (chegou com o responsável), `separated` (com adulto que não é o responsável legal) ou `unaccompanied` (sozinha).
2. Responsáveis: `POST /v1/people/{id}/guardians {guardian_id, link: declared|document_checked}`. O responsável precisa ser `verified`. `document_checked` grava quem conferiu o documento e quando.
3. `separated` ou `unaccompanied`: o servidor cria na hora o alerta `child_separated` para a fila de proteção, para a DC e para os admins do abrigo. O alerta mostra os telefones de referência do evento (Conselho Tutelar e Defesa Civil, `events.reference_contacts`) para o voluntário ligar; não há push [dec. k, Q19].
4. Mudança de `child_status` (por exemplo, o responsável chegou) é feita por `protection` ou DC.
5. Base legal: `child_protection` (art. 14, § 3º).
6. O Human Token funciona para crianças [dec. l, Q49]. Para bebês e crianças sem identificação, a miniatura continua sendo o recurso de reconhecimento (4.6).

### 3.5 Doações [dec. h, A1; dec. k, A2, A3]

#### 3.5.1 Núcleo familiar e cartão
- **Núcleo familiar** (`households/{household_id}`, UUID do cliente): membros (`member_ids`), chefe declarado (`head_id`), animais acompanhantes (`pets: [{species, count}]`, só contagem) [dec. k, A3]. Criado no check-in, online ou offline (`POST /v1/shelters/{id}/households`). Uma pessoa está em no máximo um núcleo.
- **Cartão** (`cards/{code}`): pertence ao núcleo (`household_id`). Código de 8 caracteres Crockford base32, gerado no celular, legível e digitável, mostrado como QR. Serve como cartão impresso, adesivo ou pulseira (pode ser improvisada com o código escrito à mão) [dec. k, Q44].
- Um núcleo tem no máximo um cartão `active`. Perda: o voluntário cancela e emite outro (`POST /v1/cards/{code}/cancel {replaced_by}`).
- O cartão e o núcleo valem em qualquer abrigo da rede.
- Mudança de membros: `PATCH /v1/households/{id} {add_member_ids?, remove_member_ids?, head_id?, pets?}`.

#### 3.5.2 Itens
Admin configura (`PUT /v1/shelters/{id}/items`), por item:
- `scope`: `per_person` (roupa, kit de higiene pessoal, cobertor) ou `per_household` (cesta básica, kit de limpeza) [dec. k, A2];
- período: `period_days` (padrão 7) ou `once_per_event = true` (colchão, roupa de cama) [dec. k, Q17];
- `high_value` (cartão-vale, colchão, eletrodoméstico) [dec. h, A1].

#### 3.5.3 Entrega do dia a dia
1. Voluntário lê o QR com a câmera do app (ou digita o código). O app mostra os membros do núcleo (cache local se offline) e o voluntário toca em quem está retirando.
2. `POST /v1/shelters/{id}/deliveries {id, card_code, person_id, item, quantity, delivered_at}`.
3. Servidor decide se pede liveness (online e fora do modo degradado):
   - item `high_value`;
   - sorteio: `sha256(delivery_id)` como número em [0, 1) menor que `DELIVERY_SAMPLE_RATE` (padrão 0,05) [dec. k, Q41].
   Se pede: `428 liveness_required`. O app faz captura com liveness (`purpose: "revalidation"`, `expected_person_id = person_id`) e reenvia com `capture_id`. `person_id` da captura diferente do esperado → `409 revalidation_failed`, nada é gravado.
   **Pessoa que não faz ou não pode fazer a selfie** (recusa, `no_biometrics`, sem rosto utilizável) [dec. l, Q46]: o app oferece "Aprovação do admin". Um admin do abrigo confere a pessoa pelo cartão e aprova na hora, com a própria sessão (no aparelho, pela troca de operador, 3.1.1). O app reenvia com `admin_approval: true`; o servidor exige que quem envia seja admin do abrigo (`403 admin_approval_required` se não for) e grava `verification = admin_approval`, `approved_by`, `status = ok` e a auditoria com o nome do admin. Não vai para a fila de revisão.
4. Pessoa `assisted` recebe pelo cartão como qualquer outra; o app oferece a selfie, sem exigir [dec. k, Q42 rejeitada].
5. Duplicidade [dec. k, A2]: em todos os eventos ativos [dec. c], para `per_person` procura entrega do mesmo `item` ao mesmo `person_id`; para `per_household`, ao mesmo `household_id`; dentro do período (ou no mesmo evento, se `once_per_event`). Se houver: `status = in_review` com `reasons: [{kind: duplicate_in_period, previous_delivery_id}]`. **A entrega nunca é bloqueada por duplicidade.**
6. Resposta: "Entrega registrada" e, se sinalizada, "Enviada para revisão: outra entrega em <data>". A resposta não conta nada da outra entrega (nem o abrigo, nem a data) a quem pede de um abrigo não confirmado, exceto quando a outra entrega é do próprio abrigo; a um abrigo confirmado conta a data, e o nome do outro abrigo só se ele também é confirmado. Vale também para o replay da mesma entrega e para a lista de revisão.
7. Offline (`offline: true` é uma afirmação do app: o servidor só a aceita quando `delivered_at` é pelo menos 30 s mais antigo que o recebimento, e a registra em `offline_claimed` e na auditoria) ou em modo degradado: o cartão funciona pelo cache e o sorteio não roda. Entrega de item `high_value` usa a aprovação do admin, se houver sessão de admin no aparelho; sem admin, é gravada com `verification = card` e `status = in_review`, motivo `liveness_unavailable`.
8. Admin revisa em `GET /v1/shelters/{id}/deliveries?status=in_review` e decide `reviewed_ok` ou `irregular`, com nota.

### 3.6 Registro de resgatados [dec. h, A11]
O registro é feito **no ponto de desembarque ou triagem**, não no barco.
1. Voluntário logado toca "Resgate". Foto só pela câmera do app.
2. O app grava: data e hora, GPS do ponto (`lat`, `lon`, `accuracy`), `volunteer_id`, `device_id`.
3. Opcionais: nome, pulseira, observação, nome do ponto (`landing_point`), abrigo de destino (`destination_shelter_id`).
4. A tela mostra os abrigos abertos próximos com vagas (`GET /v1/shelters/nearby`), para encaminhar [dec. k, A7].
5. Sem nome e sem pulseira: miniatura (4.6).
6. Vai pela fila: `POST /v1/rescues`. No servidor, `api+humantoken` → `person_id`; cria a pessoa como `assisted` se não existe, ou acrescenta o resgate ao histórico. Sem rosto utilizável ou em modo degradado: a pessoa é criada `no_biometrics` e a foto segue na fila como `PUT /v1/people/{id}/biometrics` (4.12). Evento: o do abrigo `open` mais próximo a ≤ `EVENT_RADIUS_KM`; senão nulo.
7. Quando a pessoa faz check-in em qualquer abrigo, o token é o mesmo e o resgate aparece no histórico.
8. Matches com buscas ativas são procurados também aqui.
9. Anomalias (4.10), com limiar próprio para resgate.

### 3.7 Busca de desaparecidos [dec. k, A5, Q8, Q45]
1. Quem registra: voluntário de abrigo `confirmed`, DC, `mediator` ou `protection` [dec. d; dec. k, A5].
2. A tela diz, antes de tudo: **"Esta busca não substitui o registro de desaparecimento na delegacia."** [dec. k, A5].
3. O operador registra: nome, apelido e observações do desaparecido; nome, telefone, parentesco e idioma da família; de 1 a 5 fotos (galeria permitida, é a exceção). Limite: `SEARCHES_PER_DAY` buscas por voluntário por dia (padrão 10), contado no dia do fuso `TZ_PILOT` (padrão `America/Sao_Paulo`): a cota recomeça à meia-noite desse fuso, e não às 21h [dec. k, Q8].
4. `POST /v1/searches`. Sem sinal, vai pela fila; as fotos ficam **cifradas no aparelho** (6.1.7) e são **apagadas logo após o envio** (`2xx`) [dec. k, Q45].
5. Servidor gera um token por foto (`api+humantoken`), guarda só os `person_id` derivados (`searches.person_ids`, nível "busca": não vira pessoa) e compara com `people`. Um match em qualquer foto conta. Em modo degradado, a busca espera na fila.
6. **A resposta é sempre a mesma**, com ou sem match: `202 {message: "search_registered", usable_photos}`. O app mostra "Busca registrada. Se a pessoa for encontrada e concordar, a equipe entrará em contato com a família." Nem o operador fica sabendo se houve match.
7. `usable_photos` diz quantas fotos geraram token, para o operador pedir outras.
8. A busca fica ativa até o descarte do evento do abrigo de origem; cada check-in ou resgate posterior é comparado com as buscas ativas [dec. k, Q25].
9. **Antes de apagar** [dec. k, A5, Q25]: quando o evento é encerrado, cada busca ativa sem match aceito gera o alerta `search_expiring` para quem a registrou, com a data do descarte e a orientação de encaminhar à delegacia e à Cruz Vermelha.
10. Match → mediação (4.8).
11. **Link da família** [decisão do dono, fase 4; opcional]: em vez de digitar a busca, o operador gera um link de uso único com validade de 12 horas (`POST /v1/search-links`). A família abre `/f/<code>` (público, sem login, PT | EN | ES) e envia os dados e de 1 a 5 fotos da própria galeria (`POST /v1/search-links/{code}/submit`), que passa pelo mesmo pipeline do item 5 e tem a mesma resposta do item 6. A busca fica em nome do operador que gerou o link (`via = family_link`), com o limite diário, a auditoria e a mediação dele; o operador não lê o conteúdo enviado. Sem fila offline; em modo degradado o envio responde `503 unico_degraded` e o link continua valendo. O link só é consumido quando pelo menos uma foto gerou token: se nenhuma serviu, o envio responde o mesmo `202` com `usable_photos: 0`, nada é registrado (nem busca nem aviso), a tentativa conta no teto de 5 por link e o link continua valendo; a página diz "Nenhuma foto serviu; mande outras fotos". O abrigo de origem da busca é o `shelter_id` do link: quem tem abrigo `confirmed` usa o seu por padrão; DC, `mediator` e `protection` escolhem qualquer abrigo `confirmed` de evento ativo (`422 invalid_shelter` se não existir ou não for confirmado; `409 event_closed` se o evento acabou).

### 3.8 Revalidação
Revalidação = nova captura com liveness cujo `person_id` resultante é comparado com o esperado (5.1).

**Entrega de doação:** só item `high_value` e sorteio (3.5.3) [dec. h, A1]; sem selfie, aprovação do admin [dec. l, Q46].

**Saída de criança** [dec. k, C2, Q18]:
1. Voluntário abre a criança e toca "Saída". Escolhe o responsável que está retirando (de `guardians`).
2. Quem pode registrar: ver matriz 2.3. Voluntário e admin só registram saída de criança `with_guardian` cujo vínculo com aquele responsável é `document_checked`; nos demais casos, só `protection` ou DC (`403 protection_role_required`).
3. Captura com liveness do adulto (`purpose: "revalidation"`, `expected_person_id = guardian_id`).
4. `POST /v1/shelters/{id}/exits {person_id, guardian_id, capture_id, exit_destination, destination_municipality?}`.
5. `person_id` da captura = `guardian_id`: saída registrada, `stay` fechada.
6. Diferente: `409 revalidation_failed`; a saída não é registrada e um alerta vai à fila de proteção. Só `protection` ou DC registram saída por exceção (`exception: {reason}`), auditada, com alerta `exceptional_exit` à DC [dec. k, Q18, C2].

**Saída de adulto** [dec. k, M5]: sem revalidação, `POST /v1/shelters/{id}/exits {person_id, exit_destination, destination_municipality?}`. `exit_destination`: `own_home`, `relatives_or_friends`, `rental_aid`, `other_shelter`, `other`, `unknown`.

### 3.9 Confirmação de abrigo [dec. d; dec. k]
**Por validador:** `POST /v1/shelters/{id}/confirm`. `confirmation_type = validator`, `can_endorse = true`.

**Aval presencial:**
1. Admin do abrigo novo gera um código: `POST /v1/shelters/{id}/endorsements/in-person/code {lat, lon, accuracy}` → `{code, expires_at}` (10 min [dec. k, Q38]), mostrado em QR.
2. Admin avalista (de abrigo `confirmed` com `can_endorse`, que não é membro ativo do abrigo novo) lê o QR com a câmera do celular, que abre `/endorse/<code>`: `POST /v1/endorsements/in-person {code, endorser_shelter_id, lat, lon, accuracy}`.
3. Servidor exige as duas `accuracy ≤ 100` e distância entre os dois celulares ≤ `SHELTER_RADIUS_M`. Confirma com `confirmation_type = in_person`, `can_endorse = true` e registra quem avalizou.

**Revogação:** validador ou DC, `POST /v1/shelters/{id}/revoke {reason}` → `unconfirmed`, `can_endorse = false`; quem avalizou fica `flagged` e gera alerta `endorser_flagged`. Efeito sobre abrigos que o revogado avalizou: seção 16.2 (Q20, adiada).

Aval remoto: seção 16.2.

### 3.10 Painel da Defesa Civil [dec. k, A7]
`GET /v1/dashboard/events` e `GET /v1/dashboard/events/{id}`. O painel diz no topo: **"Conta desabrigados (pessoas em abrigo). Desalojados (em casa de parentes ou amigos) não estão aqui."**

Por evento e por abrigo:
- abrigos: nome, confirmação, `flagged`, posição, **capacidade, lotação (`stays` abertas) e vagas**;
- pessoas únicas, **incluindo `assisted` e `no_biometrics`**, com a proporção de cada nível;
- recortes: faixa etária (0–11, 12–17, 18–59, 60+, por `birth_year`), gestantes, lactantes, pessoas com deficiência por tipo, crianças `separated` e `unaccompanied`, indígenas, migrantes;
- núcleos familiares e animais acompanhantes;
- movimentações entre abrigos (`stays` fechadas por `checkin_elsewhere` nas últimas 24 h e 7 dias) e **saídas por `exit_destination`** [dec. k, M5];
- resgates; alertas abertos; matches nas filas central e de proteção;
- indicadores do piloto: taxa de recusa da selfie [dec. k, C1]; matches `not_the_person` [dec. k, A6]; justificativas de "outro local" [dec. k, M7]; processos Unico por pessoa atendida;
- abrigos aguardando aval.

CSV diário para o boletim: `GET /v1/dashboard/events/{id}/daily.csv` (uma linha por abrigo, com as contagens acima; sem nome nem `person_id`). Contagens por agregação `count()` do Firestore.

Contatos de referência do evento (`reference_contacts: {civil_defense_phone, child_protection_phone}`) são definidos por DC ou validador (`PATCH /v1/events/{id}`) [dec. k].

---

## 4. Regras de negócio

### 4.1 Níveis de confiança das pessoas

| `level` | Origem | Libera | Transições |
|---|---|---|---|
| `verified` | Captura com liveness | Abrigo, doação, ser responsável por criança | final |
| `assisted` | Foto pela câmera do app | Abrigo, doação pelo cartão [dec. k, Q42 rejeitada], localização e reunificação | → `verified` na primeira captura com liveness, sem cadastro novo [dec. i, Q1] |
| `no_biometrics` | Registro por nome/pulseira (recusa, sem rosto, modo degradado) [dec. k, C1] | Abrigo e doação; fora da busca por foto | → `assisted` ou `verified` por `PUT /people/{id}/biometrics` (4.12) |
| "busca" | Foto da família | Nada; fica só em `searches.person_ids` | não vira pessoa |

Nível nunca desce. A decisão k atualiza a tabela original de níveis: `assisted` e `no_biometrics` recebem doação.

### 4.2 Abrigo
- `status`: `open` → `closed` (admin ou DC).
- `confirmation`: 2.2 e 3.9.
- `unconfirmed` vê só o que ele mesmo cadastrou; pessoas cadastradas nele entram normalmente na busca da rede [dec. d].
- Raio mínimo entre abrigos abertos: `SHELTER_RADIUS_M` (padrão 100) [dec. a].
- Precisão exigida: `accuracy ≤ 100 m` na criação e no aval presencial.
- `capacity` opcional (pessoas); vagas = `capacity − stays abertas`.

### 4.3 Eventos automáticos [dec. c; dec. k]
Na criação de um abrigo online (ou na sincronização de um criado offline):
1. Procura abrigos `open` de eventos `active` a ≤ `EVENT_RADIUS_KM` (padrão 50):
   - nenhum: cria evento com nome automático "Cidade/UF – mês/ano"; sem endereço ainda, "Evento – mês/ano", refeito quando o endereço chegar se `auto_named` ainda for `true`;
   - um evento: entra nele;
   - dois ou mais (**piloto: detecção e aviso** [dec. k]): entra no evento do abrigo mais próximo e gera o alerta `event_overlap` para a DC, que resolve à mão. A fusão automática de eventos está na seção 16.3.
2. Admin de qualquer abrigo do evento, ou DC, renomeia (`PATCH /v1/events/{id}`) → `auto_named = false`.
3. Atividade: qualquer escrita em abrigo do evento atualiza `events.last_activity_at`, no máximo uma vez por hora (cache em memória).
4. Encerramento automático (tarefa horária, 6.2.6): evento `active` com todos os abrigos `closed`, ou com `agora − last_activity_at ≥ INACTIVITY_DAYS` (padrão 30) → `closed`, `closed_at = agora`, `purge_at = agora + PURGE_AFTER_DAYS`.
5. Evento `closed` não reabre; abrigo de evento encerrado não aceita escritas (`409 event_closed`) [dec. k, Q36].
6. O evento organiza mas não isola: busca e duplicidade de doação olham todos os eventos ativos.
7. A pergunta "Este abrigo recebe pessoas de algum destes eventos?" (abrigo distante) fica para a seção 16.3 com a Q31.

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
- Quem vê (`GET /v1/people/{id}/thumbnail`): membros ativos do `current_shelter_id`, quem atende um match aberto sobre ela (admin, `mediator`, `protection` ou DC) e — [dec. l, Q33] — se a pessoa ainda não tem abrigo, o voluntário do ponto de desembarque que registrou o resgate e a DC. Cada acesso grava auditoria.
- Apagada na mesma escrita que preencher `name` ou `wristband`, ou quando todos os eventos da pessoa estiverem encerrados. O que vier primeiro.

### 4.7 Doações
- Duplicidade [dec. k, A2]: `(person_id, item)` para `per_person`; `(household_id, item)` para `per_household`. Janela: `period_days` ou o evento (`once_per_event`). Busca em todos os eventos ativos.
- `no_biometrics`: a mesma regra; como o `person_id` é local, na prática a checagem por pessoa fica restrita ao que foi registrado para ela [dec. k, C1].
- Duplicidade → `in_review`, nunca bloqueio.
- Liveness na entrega: item `high_value` e sorteio [dec. h, A1; dec. k, Q41]. Sem selfie possível: aprovação do admin, registrada na auditoria [dec. l, Q46].
- `verification`: `card` (padrão), `liveness` (revalidada) ou `admin_approval` (aprovada pelo admin, com `approved_by`). Motivos de revisão: `duplicate_in_period` e `liveness_unavailable` (offline ou modo degradado sem admin no aparelho).

### 4.8 Mediação da busca [dec. k, A4, A6, C2, Q24]
**Nunca devolver localização a quem buscou.** Ao criar um match (`matches/{sha256(search_id|person_id)}`, idempotente), o servidor escolhe a fila:

| Situação da pessoa encontrada | `mediator_kind` | Quem atende |
|---|---|---|
| Criança (`is_child = true`) em qualquer situação | `protection` | `protection` e DC [dec. k, C2] |
| `current_shelter_id` `confirmed` | `shelter` | Admins desse abrigo (mediador padrão) e qualquer `mediator` [dec. k, A4] |
| `current_shelter_id` `unconfirmed`, ou sem abrigo (só resgate) | `central` | `mediator` e DC [dec. d; dec. k, Q24] |

Quem decide primeiro (admin ou `mediator`) fecha o match para os demais; o papel `mediator` vale para a rede toda [dec. l, Q47].

Fluxo:
1. Vê o aviso em "Avisos" (`GET /v1/matches`): nome, apelido, parentesco e observações informados pela família; nome da pessoa no cadastro; miniatura se houver; abrigo. **Não vê o contato da família ainda.**
2. **Confere com a pessoa** os dados informados pela família antes de qualquer decisão [dec. k, A6]. A tela mostra esses dados como lista de conferência.
3. A pessoa decide; se estiver incapacitada, quem atende decide (`decided_by = operator`).
4. `POST /v1/matches/{id}/decision {decision: accept|decline|not_the_person, decided_by: person|operator, note}`.
5. `accept`: a resposta traz `family {name, phone, relationship, language}` e `whatsapp_link` (`https://wa.me/<phone>?text=<mensagem>`, no idioma da família). A tela orienta: **"Depois de combinar o contato, apague este número do seu celular."** [dec. k, A4]. Localização nunca é enviada automaticamente.
6. `decline`: nada é enviado; a busca continua ativa para outras pessoas, sem novo match para esse `person_id`.
7. `not_the_person` [dec. k, A6]: falso positivo. Encerra o match sem contar como recusa; a busca continua ativa e não gera novo match para esse `person_id`. Entra no indicador do painel.
8. Toda decisão grava auditoria com `legal_basis = vital_interest`.

### 4.9 Criança [dec. k, C2, Q19]
- `child_status`: `with_guardian`, `separated`, `unaccompanied` (3.4.1).
- `guardians: [{person_id, link: declared|document_checked, checked_by?, checked_at?}]`; responsável precisa ser `verified`.
- Revalidação na saída só vale como conferência de responsável quando o vínculo é `document_checked` (3.8).
- `separated`/`unaccompanied` → alerta `child_separated` imediato, com os telefones de referência do evento.
- Match de criança → fila `protection`.
- `legal_basis = child_protection` (art. 14, § 3º).

### 4.10 Auditoria e anomalias [dec. k, Q39]
Toda escrita e todo acesso a miniatura ou a dados de match gravam `audit_log`. No piloto, as anomalias são **só informativas**: geram alerta com `informational = true`, que aparece no painel e não na caixa de "Avisos" dos admins. Limiares:
- `anomaly_volume`: check-ins do mesmo voluntário acima de `ANOMALY_CHECKINS_PER_10MIN` (padrão 30); resgates acima de `ANOMALY_RESCUES_PER_10MIN` (padrão 100) [dec. h, A11].
- `anomaly_distance`: o mesmo `person_id` registrado em dois pontos a mais de 50 km com menos de 1 h de diferença.
- `out_of_area`: resgate a mais de `EVENT_RADIUS_KM` de todos os abrigos onde o voluntário é membro.

### 4.11 Visibilidade do histórico [dec. d; dec. k, M6, Q22]
- Abrigo `unconfirmed`: só as `stays` do próprio abrigo.
- Abrigo `confirmed`: admin vê as `stays` de outros abrigos (abrigo e datas); voluntário vê só `has_other_stays: true|false`.
- `mediator`, `protection` e DC veem o histórico da pessoa só enquanto atendem um match ou alerta sobre ela.
- Não há pesquisa livre por nome na rede [dec. k, Q22].

### 4.12 Biometria tardia (`PUT /people/{id}/biometrics`) [dec. k, A12, C1]
Usada quando uma pessoa `no_biometrics` aceita a selfie depois ou quando a foto ficou na fila em modo degradado.
1. Entrada: `capture_id` (vira `verified`) ou `photo_base64` (vira `assisted`).
2. O servidor deriva `token_person_id` (HMAC).
3. Se `people/{token_person_id}` não existe: cria-o com os dados do registro antigo, reaponta referências (`households.member_ids`, `head_id`, `deliveries.person_id`, `rescues.person_id`, `guardians`), apaga o registro antigo e guarda `former_ids: [<uuid>]`.
4. Se já existe (a pessoa estava cadastrada em outro lugar): junta as `stays` e reaponta as referências do mesmo jeito. Se as duas estiverem em núcleos diferentes, mantém o núcleo do registro com token e gera alerta `household_conflict` para os admins do abrigo atual.
5. Procura buscas ativas e cria matches.
6. Tudo numa transação por pessoa; resposta `{person_id, level}`. O app troca o ID no cache local.

---

## 5. Integração com a Unico e modo degradado

### 5.1 Flows usados

| Tela | Flow | Nível |
|---|---|---|
| Login, convite, check-in online, revalidação (entrega e saída de criança), biometria tardia com selfie | `humantoken` (Journey, com liveness) | `verified` |
| Check-in por foto, resgate, biometria tardia por foto | `api+humantoken` (sem liveness) | `assisted` |
| Busca da família | `api+humantoken` | busca |
| — | `api+liveness+humantoken*`: **não usado**; rejeita JPEG comum com `400 INVALID_CAPTURE_TOKEN` (teste de 2026-10-20) | — |
| — | `humantoken+revalidation`, `api+liveness+humantoken+revalidation`: **não usados** (13) | — |

Os termos da Unico permitem enviar foto que não veio do SDK [dec. k, Q6].

**Revalidação sem o flow de revalidação.** Nova jornada `humantoken` e comparação do `person_id` derivado. Mesmo resultado e custo do flow `+revalidation`, sem guardar o token puro (6.3.2).

**Jornada em vez de SDK** [dec. i, Q4]. **Primeira tarefa da implementação** [dec. k, Q13]: testar a jornada a partir da PWA instalada no iPhone (navegação para fora do escopo do app em modo standalone, permissão de câmera, volta pelo `callbackUri`); depois, login → criar abrigo → check-in. Plano B: SDK web da Unico.

**Resultado do teste (2026-10-21, iPhone, PWA instalada): validado; o plano B não é necessário.**
- A jornada abre numa janela do Safari por cima do app (comportamento do iOS para endereço fora do escopo). Abrir na mesma janela e em janela nova dá o mesmo resultado: a PWA usa a mesma janela.
- A câmera funciona dentro dessa janela, e a captura com liveness termina com `PROCESS_STATE_FINISHED` e token.
- O `callbackUri` dentro do escopo da PWA fecha a janela do Safari e volta para o **próprio app**, numa carga nova da página.
- Por isso o `callbackUri` carrega um identificador próprio (nonce) e a tela de retorno busca o processo por ele, sem depender do estado em memória nem do `localStorage`.
- A jornada mostra o nome da organização cadastrada no console da Unico ("O <organização> precisa das seguintes informações"). A instalação deve usar o nome "Taba" e, se possível, o logotipo, para não gerar desconfiança.

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
- `429` com mensagem `monthly process limit ... reached`: os créditos acabaram. Não repetir; entra em modo degradado com motivo `limit` (5.7) [dec. k, Q7]; alerta `unico_limit` para a DC; log `error`.
- `429` sem essa mensagem: backoff exponencial com jitter no servidor (1, 2, 4, 8 s + 0–1 s aleatório), no máximo 4 tentativas. Depois, `503 unico_busy` com `Retry-After: 30`; a fila do app reagenda (9.2).
- `500`/timeout (60 s): uma nova tentativa após 2 s; depois `502 unico_unavailable`. Três falhas seguidas → modo degradado com motivo `unavailable` por 5 min.
- `401`/`403 PERMISSION_DENIED`: `500 unico_misconfigured` e log `error`.
- Concorrência: semáforo de 4 chamadas simultâneas à Unico no processo.
- Não há sandbox: toda chamada é produção. Testes automatizados usam um servidor falso (`UNICO_URL`); chamadas reais só em teste manual.

### 5.6 Chave
`HUMAN_TOKEN_ACCESS_KEY` (formato `uk_live_...`) só no arquivo de ambiente da VM (6.4) ou no `.env` local. Nunca vai ao celular [dec. a, g]. Rotação pela API da Unico invalida a antiga na hora: atualizar o vault, rodar o `configure.yml` e reiniciar o serviço.

### 5.7 Modo degradado [dec. k, A12, Q7]
"Sem Unico" não é "sem sistema".
- Estado em `system/status {unico: ok|degraded, reason?: limit|unavailable, since, until?}`, lido por `GET /v1/status` (público, sem dados pessoais).
- `limit`: vale até o operador da instância rodar `taba unico resume` (depois de recarregar créditos). O servidor não faz chamada de teste, porque toda chamada é paga.
- `unavailable`: volta sozinho após 5 min; a próxima chamada real testa.
- Em modo degradado o servidor não chama a Unico e responde `503 unico_degraded` às rotas que dependem dela.
- No app, faixa fixa: "Reconhecimento facial indisponível. O cadastro continua por nome e pulseira."
  - Check-in e resgate seguem pelo caminho sem biometria (3.4, caminho C); se houver foto, ela fica cifrada na fila como `PUT /people/{id}/biometrics` até a Unico voltar.
  - Entrega por cartão segue normal; item `high_value` usa a aprovação do admin ou, sem admin no aparelho, vai para revisão (`liveness_unavailable`).
  - Busca espera na fila.
  - Login e convite ficam indisponíveis; quem tem sessão no aparelho continua (3.1.1).

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
- **Design system** [dec. l]: alternativa "Acolhida" — Outfit nos títulos, Figtree no texto, IBM Plex Mono nos códigos de cartão e pulseira, cantos arredondados, cores suaves, temas claro e escuro. Os tokens ficam em `design/tokens.json` e `design/tokens.css`, na raiz do repositório; a PWA importa o `tokens.css`. Esta spec não detalha o design system.
- **QR:** geração com `qrcode`; leitura com `BarcodeDetector` quando existe e `jsQR` como reserva (Safari no iOS não tem `BarcodeDetector`).
- **Rotas:** `/`, `/login`, `/return`, `/switch-operator`, `/shelters/new`, `/shelter/:id`, `/shelter/:id/checkin`, `/shelter/:id/people`, `/shelter/:id/households/:household_id`, `/shelter/:id/delivery`, `/shelter/:id/exit`, `/shelter/:id/members`, `/person/:id`, `/rescue`, `/search`, `/inbox`, `/invite/:code`, `/dashboard`, `/endorse/:code`, `/privacy`.

#### 6.1.1 IndexedDB (`taba`, versão 1)
| Store | Chave | Conteúdo |
|---|---|---|
| `config` | nome | `device_id`, `language`, `active_volunteer_id`, `sessions` (por voluntário: segredo, `expires_at`, `pin_hash`, `pin_salt`, `failed_pins`), `photo_key` (`CryptoKey` não exportável) |
| `queue` | `id` (UUID) | `{method, path, body, volunteer_id, shelter_id?, created_at, attempts, next_attempt_at, state: pending\|held\|needs_action, error?, taken_over_from?}` |
| `blobs` | `queue_id` | Fotos pendentes, **cifradas** (6.1.7), apagadas quando o item recebe `2xx` |
| `shelters` | `id` | Abrigos do voluntário (cache) |
| `people` | `id` | Pessoas do abrigo atual (cache offline por nome/pulseira), incluindo pendentes locais |
| `households` | `id` | Núcleos do abrigo atual e seus membros (cache offline) |
| `cards` | `code` | Cartões do abrigo atual (cache offline) |
| `captures` | `capture_id` | Captura em andamento (para retomar após voltar da jornada) |
| `addresses` | `shelter_id` | Coordenadas aguardando Nominatim |

#### 6.1.2 Persistência
`navigator.storage.persist()` no primeiro uso logado; resultado mostrado em "Configurações".

#### 6.1.3 Câmera
`getUserMedia({video: {facingMode: "user" | "environment"}})`, quadro em `canvas`. Sem `<input type="file">` no check-in e no resgate. Na busca, `<input type="file" accept="image/*" multiple>`.

#### 6.1.4 Redução de imagem
`canvas` → `toBlob("image/jpeg", q)`:
- Foto para token: maior lado 1280 px; qualidade começa em 0,9 e cai 0,05 até o arquivo ficar **abaixo de 800 KB** (o limite da API), para não prejudicar a qualidade na Unico [dec. k, Q34]. O tráfego de saída acima de 1 GB/mês é custo conhecido de um evento real (~US$ 0,12/GB) [dec. i, Q5].
- Miniatura: 128×128, qualidade 0,6.

#### 6.1.5 Geolocalização
`getCurrentPosition` com `enableHighAccuracy: true`; grava `accuracy` sempre.

#### 6.1.6 Nominatim [dec. k]
`GET https://nominatim.openstreetmap.org/reverse?format=jsonv2&lat=..&lon=..&accept-language=pt-BR`, no máximo 1 requisição por segundo (política do Nominatim), resultado guardado no IndexedDB. Sem sinal: guarda em `addresses` e tenta quando voltar; ao obter, `PATCH /v1/shelters/{id} {address}` pela fila.

#### 6.1.7 Fotos cifradas no aparelho [dec. k, Q45]
- Toda foto que espera na fila (busca, check-in, resgate, biometria tardia) é gravada cifrada com AES-GCM (WebCrypto), com uma `photo_key` gerada no primeiro uso como `CryptoKey` **não exportável** e guardada no IndexedDB.
- O `Blob` cifrado é apagado logo após o `2xx` do item.
- A proteção vale contra a leitura dos arquivos do navegador fora do app; não contra quem usa o próprio app desbloqueado.

### 6.2 API Rust
Um binário `taba` com os subcomandos `serve` (API + tarefa horária), `role` (papéis `validator` e `civil_defense`) e `unico resume` (sai do modo degradado por limite).

**Crates:** `axum`, `tokio`, `tower-http` (limite de corpo 6 MB, trace), `serde`/`serde_json`, `reqwest` (rustls), `firestore` (gRPC; autenticação pela conta de serviço da VM via servidor de metadados; emulador por `FIRESTORE_EMULATOR_HOST` nos testes), `geohash`, `hmac` + `sha2`, `aes-gcm`, `rand`, `base64`, `uuid`, `time`, `tracing` + `tracing-subscriber`, `anyhow`.

#### 6.2.1 Módulos
`main.rs` (CLI e roteador), `config.rs`, `auth.rs` (sessões, extrator de voluntário, checagem de papel), `unico.rs` (cliente e modo degradado), `geo.rs` (geohash e haversine), `shelters.rs`, `events.rs`, `members.rs` (convites, memberships, papéis globais), `people.rs` (check-in, saídas, miniatura, biometria tardia), `households.rs` (núcleos e cartões), `deliveries.rs`, `rescues.rs`, `searches.rs` (buscas e matches), `dashboard.rs`, `jobs.rs` (encerramento e descarte), `audit.rs` (auditoria e anomalias), `idempotency.rs`.

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
| `PILOT_RESPONSIBLE` | — | Nome e contato do responsável pelo piloto, mostrado nas telas de coleta (10.1.3) |
| `SHELTER_RADIUS_M` | 100 | [dec. a] |
| `EVENT_RADIUS_KM` | 50 | [dec. c] |
| `INACTIVITY_DAYS` | 30 | [dec. c] |
| `PURGE_AFTER_DAYS` | 30 | Biometria, pessoas e buscas [dec. k, Q9, A10] |
| `DELIVERY_RETENTION_DAYS` | 90 | Entregas pseudonimizadas [dec. k, A10; dec. l, Q50] |
| `SESSION_DAYS` | 7 | [dec. k, Q26] |
| `SEARCHES_PER_DAY` | 10 | Por voluntário [dec. k, Q8] |
| `TZ_PILOT` | `America/Sao_Paulo` | Fuso (nome IANA) cujo dia a cota diária de buscas segue |
| `DELIVERY_SAMPLE_RATE` | 0.05 | [dec. k, Q41] |
| `ANOMALY_CHECKINS_PER_10MIN` | 30 | [dec. k, Q39] |
| `ANOMALY_RESCUES_PER_10MIN` | 100 | [dec. k, Q39] |

#### 6.2.3 Formato de erro
`{"error": "<stable_code>", "detail": {...}}`. O app traduz o código; o servidor não traduz texto. Mensagens de log em inglês [dec. j].

#### 6.2.4 Concorrência e unicidade
- **Unicidade de pessoa e voluntário:** pelo ID do documento (6.3.2) em transação `get` → `create`/`update` [dec. g].
- **Criação de abrigo:** um `tokio::sync::Mutex` global no processo. `// ponytail: global lock; fine with the single e2-micro instance. With more than one instance, switch to a transaction on a per-geohash-cell lock document.`
- **Matches:** ID determinístico (`sha256(search_id|person_id)`) e `create` que falha se já existe. A decisão é gravada em transação que exige `status = open` (quem decide primeiro fecha).
- **Núcleos e cartões:** `cards/{code}` criado com `create` (falha se o código já existe; o app gera outro). "Uma pessoa em um núcleo" e "um cartão ativo por núcleo" são verificados na mesma transação.
- **Biometria tardia:** transação por pessoa (4.12).

#### 6.2.5 Autenticação
- Sessão: segredo de 32 bytes aleatórios (base64url) devolvido ao app; no Firestore, `sessions/{sha256(secret)}`. Cabeçalho `Authorization: Bearer <secret>`.
- Expira em `SESSION_DAYS`, renovada a cada requisição autenticada com mais de 1 dia desde a última renovação.
- Banimento apaga as sessões do voluntário.
- O extrator `Volunteer` carrega perfil, `global_roles` e `memberships` ativas; cada handler checa papel e estado do abrigo pela matriz 2.3.
- O PIN de troca de operador é só local (3.1.1); o servidor não o conhece.

#### 6.2.6 Tarefa horária (encerramento e descarte)
Tarefa `tokio` dentro do `serve`, a cada hora:
1. Encerra eventos (4.3.4) e gera `search_expiring` para as buscas ativas sem match aceito do evento encerrado (3.7).
2. Apaga miniaturas de pessoas cujos eventos estão todos encerrados.
3. Descarta (10.1.2) em lotes de 400 escritas, retomando na hora seguinte se passar de 15 000 exclusões no dia (camada gratuita: 20 000/dia).
4. Expira capturas, convites e códigos de aval vencidos (os que não usam TTL).
5. Sai do modo degradado `unavailable` vencido.

### 6.3 Firestore
O banco é o `(default)`, em modo nativo, na região da instalação — o único que usa a cota gratuita (`provision.yml`). Sem backup no piloto (exportação agendada e PITR têm custo) [dec. k, Q32].

#### 6.3.1 Coleções — seção 7.

#### 6.3.2 Identificadores derivados do token [dec. i, Q3]
`person_id = volunteer_id = base64url(HMAC-SHA256(HMAC_KEY, token))` (43 caracteres). O token puro não é gravado em lugar nenhum. A unicidade continua sendo pelo ID do documento [dec. g]. Pessoas `no_biometrics` usam UUID do cliente até ganharem biometria (4.12). Motivos: o formato do token não é documentado e pode conter `/` (proibido em ID do Firestore); o token é o mesmo em qualquer cliente da Unico, e o HMAC impede cruzar bases em caso de vazamento; a revalidação compara IDs derivados.
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
| `people` | `event_ids` ARRAY_CONTAINS, `level` ASC |
| `households` | `member_ids` ARRAY_CONTAINS |
| `cards` | `household_id` ASC, `status` ASC |
| `deliveries` | `person_id` ASC, `item` ASC, `delivered_at` DESC |
| `deliveries` | `household_id` ASC, `item` ASC, `delivered_at` DESC |
| `deliveries` | `shelter_id` ASC, `status` ASC, `delivered_at` DESC |
| `searches` | `person_ids` ARRAY_CONTAINS, `status` ASC |
| `searches` | `event_id` ASC, `status` ASC |
| `matches` | `mediator_kind` ASC, `mediator_shelter_id` ASC, `status` ASC, `created_at` DESC |
| `matches` | `mediator_kind` ASC, `status` ASC, `created_at` DESC (filas `central` e `protection`, sem `mediator_shelter_id`) |
| `memberships` | `volunteer_id` ASC, `status` ASC |
| `memberships` | `shelter_id` ASC, `status` ASC |
| `alerts` | `resolved` ASC, `informational` ASC, `created_at` DESC |
| `audit_log` | `shelter_id` ASC, `at` DESC |
| `audit_log` | `volunteer_id` ASC, `at` DESC |

Políticas de TTL: `sessions.expires_at`, `captures.expires_at`, `idempotency.expires_at`, `audit_log.expires_at`, `search_links.expires_at`.

#### 6.3.5 Cotas da camada gratuita (referência)
1 GiB armazenado; 50 000 leituras, 20 000 escritas e 20 000 exclusões por dia. Cada operação = 1 escrita de negócio + 1 de auditoria.

### 6.4 VM e implantação
A infraestrutura é criada e configurada com Ansible em `infra/ansible/`: `provision.yml` (APIs, Firestore, conta de serviço só com `roles/datastore.user`, IP estático, firewall 22/80/443, VM e2-micro com Debian 13 e disco `pd-standard` de 30 GB) e `configure.yml` (atualizações automáticas, Caddy, `/etc/taba/env`). O passo a passo está em **`infra/README.md`**; esta spec não o repete.

- **Inventário:** `infra/ansible/inventory/example/` (no git, valores de exemplo) e `inventory/prod/` (fora do git, valores e vault de cada instalação). Nenhum valor de instalação (projeto, IP, chaves) entra no repositório.
- **Segredos:** na VM, `/etc/taba/env` (modo 0600, diretório 0700) gerado pelo template `env.j2` a partir do vault da instalação. O deploy da API acrescenta ao `env.j2` e ao `vault.yml.example` as variáveis da 6.2.2 que faltam, com chaves de vault `vault_taba_hmac_key` e `vault_taba_thumbnail_key`. Em desenvolvimento, `.env` a partir de `.env.example` (que passa a listar todas as variáveis, sem valores).
- **Caddy:** hoje responde só um placeholder (`Caddyfile.j2`). O deploy da API troca por: `file_server` para a PWA em `/srv/taba/pwa` e `reverse_proxy /v1/* 127.0.0.1:8080`, com os cabeçalhos `Strict-Transport-Security`, `Content-Security-Policy` (`default-src 'self'; connect-src 'self' https://nominatim.openstreetmap.org; img-src 'self' blob: data:`), `X-Content-Type-Options`, `Referrer-Policy: same-origin`, `Permissions-Policy: camera=(self), geolocation=(self)`.
- **systemd:** `taba.service` (`ExecStart=/usr/local/bin/taba serve`, `EnvironmentFile=/etc/taba/env`, `Restart=always`, `DynamicUser=yes`), instalado por uma tarefa nova do `configure.yml`.
- **Swap:** 1 GB (a VM tem 1 GB de RAM), também pelo `configure.yml`.
- **Build:** o binário é compilado fora da VM (`x86_64-unknown-linux-gnu`) e copiado; a e2-micro não tem memória para compilar Rust.
- A API confia em `X-Forwarded-For` só vindo de `127.0.0.1`.
- Custo: camada gratuita, menos o IPv4 externo (~US$ 3/mês) [dec. g] e a saída acima de 1 GB/mês [dec. i, Q5]. Para um evento real, o dimensionamento da VM precisa ser revisto [dec. k, A12].

---

## 7. Modelo de dados (Firestore)

Datas: `Timestamp`. `client_*` = hora do celular; sem prefixo = hora do servidor.

### `system/status`
`unico: ok|degraded`, `reason?: limit|unavailable`, `since`, `until?`.

### `events/{event_id}` (UUID do servidor)
`name`, `auto_named: bool`, `status: active|closed|purged`, `created_at`, `last_activity_at`, `closed_at?`, `closure_reason?: all_shelters_closed|inactivity`, `purge_at?`, `delivery_purge_at?`, `purged_at?`, `reference_contacts: {civil_defense_phone?, child_protection_phone?}`, `stats?: {shelters, unique_people, households, rescues, deliveries}` (gravadas no descarte).

### `shelters/{shelter_id}` (UUID do cliente)
`name`, `lat`, `lon`, `accuracy_m`, `geohash`, `address: {text, city, state, pending: bool}`, `event_id?`, `capacity?`, `created_by`, `client_created_at`, `created_at`, `status: open|closed`, `confirmation: unconfirmed|confirmed`, `confirmation_type?: validator|in_person`, `can_endorse: bool`, `endorsements: [{kind: in_person, volunteer_id, endorser_shelter_id, at, distance_m}]`, `revocations: [{by, at, reason}]`, `flagged: bool`, `other_site_reason?`, `conflicts_with?`, `items: {<name>: {scope: per_person|per_household, period_days?, once_per_event: bool, high_value: bool}}`.

### `volunteers/{volunteer_id}` (HMAC do token)
`name`, `phone?`, `show_phone: bool`, `language: pt-BR|en`, `global_roles: [validator|civil_defense|mediator|protection]`, `role_grants: [{role, by, at}]`, `banned: bool`, `banned_by?`, `banned_at?`, `flagged: bool`, `flag_reasons: []`, `created_at`, `last_seen_at`.

### `memberships/{shelter_id}_{volunteer_id}`
`shelter_id`, `volunteer_id`, `role: admin|volunteer`, `status: active|removed|left`, `via: creator|qr|link`, `invite_code?`, `invited_by?`, `joined_at`, `removed_by?`, `removed_at?`.

### `invites/{code}` (128 bits aleatórios, base64url)
`shelter_id`, `kind: qr|link`, `role: admin|volunteer` (`link` ⇒ `volunteer`), `created_by`, `created_at`, `expires_at`, `status: active|used|canceled`, `used_by?`, `used_at?`.

### `endorsement_codes/{code}`
`shelter_id`, `created_by`, `lat`, `lon`, `accuracy_m`, `expires_at`, `used: bool`.

### `sessions/{sha256(secret)}`
`volunteer_id`, `device_id`, `created_at`, `renewed_at`, `expires_at` (TTL).

### `captures/{capture_id}` (UUID do servidor)
`process_id` (Unico), `purpose: login|invite|checkin|revalidation|biometrics`, `device_id`, `volunteer_id?`, `shelter_id?`, `expected_person_id?`, `status: created|ok|inconclusive|failed|canceled`, `person_id?` (HMAC; nunca o token), `consumed: bool`, `created_at`, `expires_at` (15 min, TTL).

### `people/{person_id}` (HMAC do token; UUID do cliente se `no_biometrics`)
- Identidade e nível: `level: verified|assisted|no_biometrics`, `biometrics_declined: bool`, `former_ids: []`, `name?`, `social_name?`, `wristband?`, `birth_year?`.
- Criança: `is_child: bool`, `child_status?: with_guardian|separated|unaccompanied`, `guardians: [{person_id, link: declared|document_checked, checked_by?, checked_at?}]`.
- Necessidades e vulnerabilidades: `needs: {continuous_medication, dietary_restriction, reduced_mobility, health_follow_up}` (bools), `vulnerabilities: {pregnant, lactating, disability: [], indigenous_people?, migrant_language?}`.
- Família: `household_id?`.
- Passagens: `current_shelter_id?`, `stays: [{shelter_id, event_id, entered_at, left_at?, exit_reason?: checkin_elsewhere|exit|exceptional_exit, exit_destination?: own_home|relatives_or_friends|rental_aid|other_shelter|other|unknown, destination_municipality?, registered_by}]`, `event_ids: []`, `rescue_ids: []`.
- Outros: `thumbnail_encrypted?: bytes`, `origin: checkin|rescue`, `legal_basis`, `created_at`, `updated_at`, `client_edited_at`.

### `households/{household_id}` (UUID do cliente)
`member_ids: []`, `head_id?`, `pets: [{species, count}]`, `created_by`, `created_shelter_id`, `client_created_at`, `created_at`, `updated_at`.

### `cards/{code}` (8 caracteres Crockford base32, gerado no cliente)
`household_id`, `shelter_id` (emissor), `status: active|canceled`, `issued_by`, `client_issued_at`, `created_at`, `canceled_at?`, `replaced_by?`.

### `deliveries/{delivery_id}` (UUID do cliente)
`shelter_id`, `shelter_name`, `event_id`, `person_id?`, `household_id?`, `household_pseudonym?`, `card_code?`, `item`, `scope`, `quantity`, `delivered_by`, `delivered_at` (cliente), `received_at`, `verification: card|liveness|admin_approval`, `approved_by?`, `status: ok|in_review|reviewed_ok|irregular`, `reasons: [{kind: duplicate_in_period|liveness_unavailable, previous_delivery_id?}]`, `reviewed_by?`, `review_note?`, `pseudonymized: bool`.

### `rescues/{rescue_id}` (UUID do cliente)
`person_id`, `volunteer_id`, `device_id`, `lat`, `lon`, `accuracy_m`, `landing_point?`, `captured_at` (cliente), `received_at`, `event_id?`, `destination_shelter_id?`, `note?`.

### `searches/{search_id}` (UUID do cliente)
`created_by` (o voluntário em nome de quem a busca está: quem a digitou ou quem gerou o link da família), `via: volunteer|family_link`, `origin_shelter_id?`, `event_id?`, `person_ids: []` (HMAC dos tokens das fotos), `photos_sent`, `usable_photos`, `missing_person: {name, nickname?, notes}`, `family: {name, phone, relationship, language}`, `status: active|closed`, `expiry_notified_at?`, `created_at`.

### `search_links/{code}` (128 bits aleatórios, base64url)
`created_by`, `shelter_id`, `event_id?`, `status: active|used|canceled`, `created_at`, `expires_at` (12 h; TTL), `used_at?`, `search_id?`, `attempts`.

### `matches/{sha256(search_id|person_id)}`
`search_id`, `person_id`, `is_child: bool`, `mediator_kind: shelter|central|protection`, `mediator_shelter_id?`, `status: open|accepted|declined|not_the_person`, `decided_by?: person|operator`, `decided_by_volunteer?`, `decided_at?`, `note?`, `created_at`.

### `alerts/{alert_id}`
`kind: anomaly_volume|anomaly_distance|out_of_area|volunteer_removed|shelter_flagged|event_overlap|unico_limit|endorser_flagged|exceptional_exit|child_separated|search_expiring|family_search_received|household_conflict|revalidation_failed`, `informational: bool`, `target_kind`, `target_id`, `event_id?`, `shelter_ids: []`, `volunteer_ids: []`, `for_civil_defense: bool`, `for_protection: bool`, `detail`, `created_at`, `resolved: bool`, `resolved_by?`.

### `audit_log/{auto}`
`at`, `volunteer_id?`, `device_id?`, `ip?`, `shelter_id?`, `action`, `target_kind`, `target_id`, `legal_basis?`, `lat?`, `lon?`, `taken_over_from?`, `expires_at` (TTL de 1 ano [dec. k, Q21]). Sem nome nem foto.

### `idempotency/{key}`
`volunteer_id?`, `route`, `http_status`, `body`, `expires_at` (7 dias, TTL).

`legal_basis` (enum): `consent` (art. 11, I) | `vital_interest` (art. 11, II, "e") | `fraud_prevention` (art. 11, II, "g") | `public_policy` (art. 11, II, "b") | `child_protection` (art. 14, § 3º) | `family_consent` (art. 7º, I).

---

## 8. API REST

Base `https://<domain>/v1`. JSON. Autenticação `Authorization: Bearer <session>` exceto onde indicado "público". Rotas que escrevem aceitam `Idempotency-Key` (o app usa o `id` do item da fila) e `X-Device-Id`; chave repetida devolve a resposta gravada em `idempotency`. Rotas que dependem da Unico respondem `503 unico_degraded` em modo degradado.

### 8.1 Status, capturas e sessão
| Método e rota | Auth | Corpo | Resposta |
|---|---|---|---|
| `GET /status` | público | — | `{unico: ok\|degraded, reason?}` |
| `POST /captures` | público para `login` e `invite`; sessão para as demais | `{purpose, shelter_id?, expected_person_id?}` | `201 {capture_id, link, expires_at}` |
| `POST /sessions` | público | `{capture_id, profile?: {name, phone?, show_phone, language}}` | `201 {session, expires_at, volunteer}`; `202 capture_pending`; `422 profile_required`; `403 banned` |
| `DELETE /sessions/current` | sessão | — | `204` |
| `GET /me` | sessão | — | `{volunteer, global_roles, memberships: [{shelter_id, name, role, confirmation}]}` |
| `PATCH /me` | sessão | `{name?, phone?, show_phone?, language?}` | `200` |

### 8.2 Abrigos e eventos
| Método e rota | Corpo | Resposta |
|---|---|---|
| `POST /shelters` | `{id, name, lat, lon, accuracy, address?, capacity?, other_site_reason?, client_created_at}` | `201 {shelter, event?}`; `409 shelter_nearby {shelter_id, name, distance_m, contact}`; `422 insufficient_accuracy` |
| `GET /shelters/{id}` | — | abrigo (membros ativos; outros voluntários veem só nome, confirmação e vagas) |
| `GET /shelters/nearby?lat&lon` | — (qualquer voluntário) | `{shelters: [{id, name, distance_m, capacity, occupancy, vacancies, confirmation}]}`, do mais perto ao mais longe (abertos, de evento ativo, a ≤ `EVENT_RADIUS_KM`) |
| `PATCH /shelters/{id}` | `{name?, address?, capacity?}` (admin; DC para `capacity`) | `200` |
| `POST /shelters/{id}/close` | — (admin ou DC) | `200` |
| `PUT /shelters/{id}/items` | `{items: {<name>: {scope, period_days?, once_per_event, high_value}}}` (admin) | `200` |
| `PATCH /events/{id}` | `{name?, reference_contacts?}` (`name`: admin de abrigo do evento ou DC; `reference_contacts`: Val ou DC) | `200` |

### 8.3 Confirmação
| Método e rota | Corpo | Resposta |
|---|---|---|
| `POST /shelters/{id}/confirm` | — (Val, DC) | `200` |
| `POST /shelters/{id}/revoke` | `{reason}` (Val, DC) | `200` |
| `POST /shelters/{id}/endorsements/in-person/code` | `{lat, lon, accuracy}` (admin do abrigo novo) | `201 {code, expires_at}` |
| `POST /endorsements/in-person` | `{code, endorser_shelter_id, lat, lon, accuracy}` | `200 {shelter}`; `403 cannot_endorse`; `422 too_far {distance_m}` |

### 8.4 Convites, membros e papéis
| Método e rota | Corpo | Resposta |
|---|---|---|
| `POST /shelters/{id}/invites` | `{kind: qr\|link, role}` (admin) | `201 {code, url, expires_at}`; `422 link_volunteer_only` |
| `GET /shelters/{id}/invites` | — (admin) | ativos e quem entrou por link |
| `DELETE /invites/{code}` | — (admin) | `204` |
| `GET /invites/{code}` | público | `{shelter_name, role, confirmed, valid}` |
| `POST /invites/{code}/accept` | público `{capture_id, profile?}` | `201 {session, membership}`; `410 invite_invalid`; `409 already_member`; `403 banned\|removed_from_shelter` |
| `GET /shelters/{id}/members` | — (membro) | lista |
| `PATCH /shelters/{id}/members/{volunteer_id}` | `{role: admin}` (admin) | `200` |
| `DELETE /shelters/{id}/members/{volunteer_id}` | — (admin remove voluntário; DC remove qualquer um) | `204`; `403 admin_cannot_remove_admin`; `409 last_admin` |
| `POST /shelters/{id}/leave` | — | `204`; `409 last_admin` |
| `POST /volunteers/{volunteer_id}/ban` | `{reason}` (DC) | `200` |
| `POST /volunteers/{volunteer_id}/roles` | `{role: mediator\|protection}` (Val, DC) | `200` |
| `DELETE /volunteers/{volunteer_id}/roles/{role}` | — (Val, DC) | `204` |

### 8.5 Pessoas, núcleos e cartões
| Método e rota | Corpo | Resposta |
|---|---|---|
| `POST /shelters/{id}/checkins` | `{id, capture_id? \| photo_base64? \| no_biometrics: true, biometrics_declined?, data: {name?, social_name?, wristband?, birth_year?, is_child, child_status?, needs?, vulnerabilities?}, thumbnail_base64?, legal_basis: consent\|vital_interest\|child_protection, client_created_at}` | `201 {person_id, level, is_new, history}`; `422 no_face\|capture_inconclusive\|invalid_image` |
| `GET /shelters/{id}/people` | — (membro) | pessoas com `current_shelter_id = id` (histórico por 4.11) |
| `GET /shelters/{id}/people/needs-selfie` | — | `assisted` e `no_biometrics` sem recusa no abrigo |
| `GET /people/{person_id}` | — (4.11) | pessoa |
| `PATCH /people/{person_id}` | `{data, client_edited_at}` | `200`; apaga a miniatura se `name` ou `wristband` vierem preenchidos; `child_status` só por `protection` ou DC |
| `PUT /people/{person_id}/biometrics` | `{capture_id? \| photo_base64?}` | `200 {person_id, level}`; `422 no_face\|capture_inconclusive` |
| `POST /people/{person_id}/guardians` | `{guardian_id, link: declared\|document_checked}` | `200`; `422 guardian_not_verified` |
| `GET /people/{person_id}/thumbnail` | — (4.6) | `image/jpeg`, auditado |
| `POST /shelters/{id}/exits` | `{person_id, exit_destination, destination_municipality?, guardian_id?, capture_id?, exception?: {reason}}` | `200`; `409 revalidation_failed`; `403 protection_role_required` |
| `POST /shelters/{id}/households` | `{id, member_ids, head_id?, pets?, client_created_at}` | `201`; `409 person_in_other_household` |
| `GET /households/{id}` | — (membro de abrigo onde algum membro está) | `{id, members: [{person_id, name, level}], head_id, pets, card_code?}` |
| `PATCH /households/{id}` | `{add_member_ids?, remove_member_ids?, head_id?, pets?}` | `200`; `409 person_in_other_household` |
| `POST /households/{id}/cards` | `{code, client_issued_at}` | `201`; `409 code_taken\|household_has_card` |
| `GET /cards/{code}` | — (membro de qualquer abrigo) | `{code, status, household_id, members: [{person_id, name, level}]}` (sem abrigo atual nem histórico) |
| `POST /cards/{code}/cancel` | `{replaced_by?}` | `200` |

### 8.6 Entregas
| Método e rota | Corpo | Resposta |
|---|---|---|
| `POST /shelters/{id}/deliveries` | `{id, card_code, person_id, item, quantity, delivered_at, capture_id?, admin_approval?: true}` | `201 {delivery_id, status, reasons}`; `428 liveness_required`; `409 revalidation_failed`; `403 admin_approval_required`; `404 card_not_found\|person_not_in_household` |
| `GET /shelters/{id}/deliveries?status=` | — (admin) | lista |
| `POST /deliveries/{id}/review` | `{decision: reviewed_ok\|irregular, note}` (admin) | `200` |

Na resposta de `POST /shelters/{id}/deliveries`, cada item de `reasons` traz `kind`, `previous_delivery_id?`, `previous_delivered_at?` e `previous_shelter_name?` (conforme a regra do item 6 de 3.5.3, 4.7). `PUT /shelters/{id}/items` responde `{items}`; `GET /shelters/{id}/deliveries` responde `{deliveries: [...]}` com nomes (`person_name`, `delivered_by_name`, `approved_by_name`), nunca o `person_id`.

Detalhes das entregas: o corpo aceita `offline: true` (entrega feita sem sinal ou com a Unico fora: sem sorteio; `high_value` sem `admin_approval` vira `in_review` com `liveness_unavailable`; o servidor aplica a mesma regra em modo degradado). `428 liveness_required` traz `detail: {reason: high_value|sample, admin_approval_only}`, e `admin_approval_only` é `true` para pessoa sem biometria (a selfie não resolve, só o admin). Reenviar o mesmo `id` com o mesmo voluntário e abrigo devolve a mesma resposta; outro voluntário ou abrigo → `409 delivery_id_taken`. A revalidação que não confere consome a captura, não grava a entrega e audita `delivery.revalidation_failed`. A duplicidade considera entregas a menos de `period_days` uma da outra, em qualquer ordem (a fila offline pode chegar fora de ordem). Uma entrega que chega com um `person_id` antigo (biometria tardia, 4.12) vale para o membro do núcleo que o tem em `former_ids`. A auditoria (`delivery.create`, `delivery.admin_approval`, base `fraud_prevention`) guarda o `volunteer_id` de quem enviou (o admin, na aprovação); o nome sai do cadastro. `GET /shelters/{id}/deliveries` exige `status`. `GET /shelters/{id}` traz `items` para membros, validador e Defesa Civil. `delivered_at` (o relógio do celular) mais velho que 30 dias é recusado (`422 invalid_request`, `detail.field = delivered_at`); um horário no futuro não é recusado, para que um celular com o relógio adiantado não trave a fila: o servidor o limita ao horário de recebimento e grava `delivered_at_clamped: true` (a ação da auditoria ganha o sufixo `_clamped`). O corte de 30 s do `offline` e a duplicidade usam o valor já limitado. `admin_approval` só vale onde a selfie seria pedida (`high_value` ou sorteada, ou `high_value` offline ou em modo degradado); fora disso a entrega é comum (`verification = card`, sem `approved_by`). `previous_delivered_at` e `previous_shelter_name` seguem a regra do item 6 de 3.5.3. `POST /deliveries/{id}/review` só decide entrega `in_review` (`409 delivery_not_in_review`, `404 delivery_not_found`).

### 8.7 Resgates, buscas e matches
| Método e rota | Corpo | Resposta |
|---|---|---|
| `POST /rescues` | `{id, photo_base64?, lat, lon, accuracy, captured_at, data?: {name?, wristband?, note?}, landing_point?, destination_shelter_id?, thumbnail_base64?}` | `201 {rescue_id, person_id, level}` |
| `PATCH /rescues/{id}` | `{landing_point?, destination_shelter_id?}` (autor) | `200` |
| `POST /searches` | `{id, photos_base64: [1..5], missing_person: {name, nickname?, notes}, family: {name, phone, relationship, language}}` | `202 {message: "search_registered", usable_photos}` (igual com ou sem match); `429 search_limit` |
| `GET /matches` | — | filas que o voluntário atende (4.8) |
| `GET /matches/{id}` | — | detalhes para conferência, sem contato da família |
| `POST /matches/{id}/decision` | `{decision: accept\|decline\|not_the_person, decided_by: person\|operator, note?}` | `200 {family?, whatsapp_link?}`; `409 match_already_decided` |

### 8.8 Painel, alertas, exportação
| Método e rota | Auth | Resposta |
|---|---|---|
| `GET /dashboard/events` | Val, DC | eventos com contagens |
| `GET /dashboard/events/{id}` | Val, DC | 3.10 |
| `GET /dashboard/events/{id}/daily.csv` | Val, DC | CSV diário para o boletim (3.10) |
| `GET /alerts` | admin (dos seus abrigos, não informativos), `protection` (de proteção), DC (todos) | lista |
| `POST /alerts/{id}/resolve` | destinatário | `200` |
| `GET /shelters/{id}/export.csv` | admin de abrigo `confirmed` | CSV das pessoas, núcleos e entregas do próprio abrigo, sem miniatura nem `person_id`, auditado [dec. k, Q22] |

---

## 9. Sincronização offline e conflitos

### 9.1 O que funciona offline
Criar abrigo, check-in por foto (`assisted`) ou sem biometria, núcleo familiar e cartão, edição de dados de pessoa, saída de adulto, entrega por cartão, resgate, busca [dec. k, Q45], atualização de endereço, troca de operador por PIN. **Não funciona offline:** login, convites, capturas com liveness (inclui entrega de alto valor revalidada e saída de criança), aval, confirmação, mediação, painel.

### 9.2 Fila
- Cada escrita vira um item em `queue` com `id` UUID, usado como `Idempotency-Key` e, quando a entidade é nova, como ID do documento.
- Drenagem: ao abrir o app, no evento `online`, logo após enfileirar e a cada 30 s enquanto houver pendentes e o app estiver aberto (iOS não tem Background Sync).
- FIFO, um item por vez. Por resultado:
  - `2xx` ou resposta idempotente: remove o item e o `Blob` cifrado; atualiza caches.
  - `401`: para a drenagem e pede login. Itens de outro voluntário só seguem com a sessão dele (troca por PIN) ou se um admin os assumir.
  - **Assumir itens** [dec. k, M1]: um admin do abrigo, com sessão ativa no aparelho, toca "Assumir pendências de <nome>". O app troca o `volunteer_id` dos itens e grava `taken_over_from`; o servidor registra a auditoria com esse campo.
  - `409 shelter_nearby` na criação: o item e todos os do mesmo `shelter_id` ficam `held` até o admin explicar (3.2.1).
  - `422` de imagem/rosto: `needs_action`; o app lista em "Pendências".
  - `503 unico_degraded`: o item fica `pending` e só volta a ser tentado quando `GET /status` voltar a `ok`.
  - `429`, `5xx`, `503`, rede: `attempts += 1`, `next_attempt_at = agora + min(5 s × 2^attempts, 10 min) × (0,5 a 1,5 aleatório)`; respeita `Retry-After`.
  - Outros `4xx`: `needs_action` com o código.
- Contador "N pendentes" sempre visível; aviso se houver item pendente há mais de 24 h.

### 9.3 Dependências
Itens que referenciam um abrigo, pessoa ou núcleo criado offline vêm depois dele (FIFO). Se o abrigo estiver `held`, os dependentes também ficam.

### 9.4 Resolução de conflitos (o servidor é a fonte da verdade [dec. a])
| Caso | Regra |
|---|---|
| Mesma pessoa cadastrada em dois celulares (offline) | Mesmo token → mesmo `person_id`. O segundo check-in vira atualização: acrescenta `stay`; nível só sobe. |
| Mesma pessoa em dois abrigos ao mesmo tempo | A `stay` com `entered_at` mais recente vira `current_shelter_id`; a outra fecha com `exit_reason = checkin_elsewhere`. Entradas a menos de 1 h e abrigos a > 50 km: alerta `anomaly_distance`. |
| Mesma pessoa em dois núcleos criados offline | O primeiro a sincronizar fica; o segundo recebe `409 person_in_other_household` → `needs_action` ("pessoa já está no núcleo X"). |
| Dois cartões para o mesmo núcleo | O segundo recebe `409 household_has_card` → `needs_action`. |
| Registro `no_biometrics` que ganha token de pessoa já cadastrada | 4.12. |
| Edição de dados da pessoa | Último a editar ganha, por `client_edited_at`; edição mais antiga é ignorada (`200` com o estado atual). |
| Item repetido (retentativa) | `Idempotency-Key` devolve a resposta original; nenhuma nova chamada à Unico. |
| Dois abrigos a < 100 m criados offline | 3.2.1 (piloto: explicação e DC). |
| Dois eventos alcançados pelo mesmo abrigo | 4.3 (piloto: alerta `event_overlap`). |
| Escrita para abrigo de evento encerrado | `409 event_closed` → `needs_action` [dec. k, Q36]. |

---

## 10. Segurança e privacidade

### 10.1 LGPD

#### 10.1.1 Base legal por operação (gravada em `audit_log.legal_basis`)
| Operação | `legal_basis` |
|---|---|
| Check-in de adulto consciente (com ou sem selfie) | `consent` (art. 11, I) |
| Check-in de pessoa ferida/incapacitada, resgate, miniatura | `vital_interest` (art. 11, II, "e") |
| Check-in e saída de criança, alertas de criança | `child_protection` (art. 14, § 3º) |
| Entrega de doação e revalidação | `fraud_prevention` (art. 11, II, "g") |
| Busca de desaparecido e mediação | `vital_interest` |
| Dados de contato da família | `family_consent` (art. 7º, I) |
| Credenciamento de voluntário | `fraud_prevention` [dec. k, Q35] |
| Painel e operação da Defesa Civil | `public_policy` (art. 11, II, "b") |

#### 10.1.2 Descarte diferenciado [dec. k, A10, Q9, A5]
- **Evento encerrado:** apaga miniaturas na hora [dec. e]; avisa quem registrou buscas ativas sem match (3.7).
- **`purge_at = closed_at + PURGE_AFTER_DAYS` (30):** apaga pessoas cujos eventos em `event_ids` estão todos vencidos (incluindo os IDs derivados do token e os registros `no_biometrics`), núcleos, cartões, resgates, buscas (com os `person_ids` derivados), matches, alertas, memberships e abrigos do evento. **Entregas** são pseudonimizadas: apagam-se `person_id` e `card_code`, e `household_id` vira `household_pseudonym` (valor aleatório por núcleo, sem ligação com pessoa); ficam abrigo (nome), item, escopo, quantidade, data e status.
- **`delivery_purge_at = closed_at + DELIVERY_RETENTION_DAYS`:** apaga as entregas pseudonimizadas. No piloto, 90 dias [dec. l, Q50]; com controlador, o prazo de prestação de contas passa a ser dele.
- **Evento:** fica só com `name`, datas e `stats`, `status = purged`.
- **Perfis de voluntário:** apagados 365 dias após `last_seen_at`, exceto banidos [dec. k, Q21].
- **Auditoria:** TTL de 1 ano [dec. k, Q21].
- **Fotos de pessoas** nunca ficam no servidor: só existem na memória do processo durante a chamada à Unico [dec. e]. No celular, ficam cifradas e são apagadas após o `2xx` (6.1.7).

#### 10.1.3 Transparência [dec. k, C1, M2]
- Texto curto em cada tela de coleta (check-in, busca): finalidade, prazo de guarda, quem acessa, base legal, responsável.
- Responsável: no piloto não há controlador definido [dec. k, Q10]; as telas mostram `PILOT_RESPONSIBLE`, o responsável pelo piloto definido na instalação [dec. l, Q51].
- Frases obrigatórias no check-in: "Recusar a selfie não tira abrigo nem doação" e "Os dados não são compartilhados com polícia nem com imigração".
- Versão curta para leitura em voz alta. Consentimento em PT, EN e ES [dec. h, M3].
- Na busca: "Esta busca não substitui o registro de desaparecimento na delegacia" [dec. k, A5].
- Página `/privacy` com o texto completo.

#### 10.1.4 RIPD, região e transferência internacional
- O repositório deve conter `docs/pt_br/RIPD.md` antes de uso com dados reais (art. 38): dados tratados (ID derivado do token, miniatura, nome, nome social, vulnerabilidades, necessidades operacionais, núcleo familiar, localização, cartão, contato da família), finalidades, bases (10.1.1), riscos (agressor, abrigo falso, operador mal-intencionado, vazamento, falso positivo de match, coerção ao consentimento), medidas (esta seção), descarte.
- Região [dec. h, A8]: o piloto fica nos EUA (`us-east1`). O RIPD registra a transferência internacional (art. 33) e documenta que cada controlador pode subir a própria instância em `southamerica-east1` (São Paulo), ao custo de sair da camada gratuita, sem mudança de código: só o inventário Ansible muda.
- Controlador e mecanismo do art. 33: definidos quando houver uso com uma organização real [dec. k, Q10, A9].

### 10.2 Proteção contra abrigo falso
- Nasce `unconfirmed`: vê só o que cadastrou, não busca, não media, não exporta [dec. d].
- Tela de consentimento mostra "Abrigo não confirmado pela Defesa Civil".
- Criação exige perfil com liveness: o criador é identificável e banível.
- No piloto, só validador ou aval presencial confirmam [dec. k]. Revogação sinaliza avalistas.
- Abrigo a < 100 m de outro: bloqueado ou `flagged` para a DC.

### 10.3 Proteção contra agressor que busca a vítima
- Busca só por operador de abrigo confirmado, DC, `mediator` ou `protection`; limite diário por voluntário [dec. k, Q8, A5].
- Resposta da busca idêntica com ou sem match.
- Quem media é do abrigo onde a pessoa está, `mediator`, `protection` ou DC — nunca o abrigo de quem buscou.
- Mediador confere os dados com a pessoa antes de aceitar; falso positivo vira `not_the_person` [dec. k, A6].
- Contato da família só aparece depois de a pessoa aceitar, com orientação para apagá-lo depois [dec. k, A4].
- Localização nunca é enviada automaticamente.
- Voluntário comum não vê por quais abrigos a pessoa passou [dec. k, M6].
- Leitura de cartão e núcleo devolve só nomes e nível dos membros, sem abrigo atual nem histórico.
- Busca grava auditoria com quem buscou.
- Risco residual: agressor que é admin do mesmo abrigo da vítima. Mitigação: `mediator` pode atender a fila do abrigo (4.8) e auditoria; registrado no RIPD.

### 10.4 Endpoints públicos e custo
- Públicos no piloto: `GET /status`, `POST /captures` (só `login` e `invite`), `POST /sessions`, `GET /invites/{code}`, `POST /invites/{code}/accept`, `GET /search-links/{code}` e `POST /search-links/{code}/submit` (link da família, 3.7 item 11: link de uso único de 12 h, limite por link e por aparelho).
- Limites para capturas públicas ficam com a seção 16 (Q15, adiada). No piloto, abuso só consome créditos, e o fim dos créditos leva ao modo degradado [dec. k, Q7].
- Corpo máximo de 6 MB (5 fotos de até 800 KB em base64); imagem individual ≤ 800 KB.
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
- Mensagem de WhatsApp à família: no `family.language` informado na busca (`pt-BR`, `en` ou `es`).
- Datas e números: `Intl.DateTimeFormat` e `Intl.NumberFormat`.
- Dados digitados não são traduzidos. Erros do servidor chegam como códigos estáveis e são traduzidos pelo app.

### 11.2 O que fica em inglês
Identificadores, rotas, logs, commits e PRs (seção 0) [dec. j].

### 11.3 Espanhol [dec. h, M3; dec. k, Q43]
Na v1, só a tela de consentimento do check-in (incluindo a versão curta para leitura em voz alta). Ela tem seletor próprio "PT | EN | ES", independente do idioma do voluntário (quem lê o consentimento é a pessoa atendida); o padrão é o idioma da interface. A página do "Estou Seguro" em espanhol vai junto com ela na fase posterior (16.1).

---

## 12. Questões abertas

**Não há pendências do dono nesta versão.** Q33 e Q46–Q51 foram fechadas no item l (seção 13). As questões adiadas com o escopo pós-piloto (Q15, Q20, Q29, Q30, Q31, Q40) estão na seção 16 e voltam quando essas fases forem planejadas.

---

## 13. Registro de decisões

### 13.1 Decisões do dono
| Decisão | Origem | Onde |
|---|---|---|
| Check-in offline nasce `assisted` e sobe na primeira captura com liveness | Q1 [dec. i] | 3.4, 4.1 |
| Fusão de abrigos como pedido aceito pelo destino (adiada para depois do piloto) | Q2 [dec. i, k] | 16.3 |
| HMAC do token, nunca o token puro; `HMAC_KEY` no vault, com backup | Q3 [dec. i] | 6.3.2, 10.5 |
| Liveness pela jornada; plano B SDK web | Q4 [dec. i] | 5.1 |
| Saída acima de 1 GB/mês é custo conhecido | Q5 [dec. i] | 6.1.4, 6.4 |
| Termos da Unico permitem foto que não veio do SDK | Q6 [dec. k] | 5.1 |
| Fim dos créditos: chamadas param, app segue em modo degradado | Q7 [dec. k] | 5.5, 5.7 |
| Busca por operador de abrigo confirmado, DC, `mediator` ou `protection`; 5 fotos; 10 por voluntário por dia; aviso de que não substitui o registro policial | Q8 + A5 [dec. k] | 3.7 |
| Descarte diferenciado: biometria e buscas em 30 dias; entregas pseudonimizadas pelo prazo do controlador | Q9 + A10 [dec. k] | 10.1.2 |
| Piloto sem controlador definido | Q10, A9 [dec. k] | 1.1, 10.1.3, 10.1.4 |
| Licença AGPL-3.0 | Q11 | `LICENSE` |
| Repositório público `github.com/thingsbuilder/taba` | Q12 | — |
| Primeira tarefa: teste da jornada no iPhone; depois login → criar abrigo → check-in | Q13 [dec. k] | 5.1 |
| Domínio `taba.thingsbuilder.com`; projeto Taba; região `us-east1` | Q14 [dec. g] | 6, 6.4 |
| Duplicidade por pessoa ou por família, conforme o item | Q16 + A2 [dec. k] | 3.5.2, 4.7 |
| Período por item, incluindo "uma vez por evento" | Q17 [dec. k] | 3.5.2 |
| Saída de criança: revalidação falhou bloqueia; exceção só por `protection` ou DC | Q18 + C2 [dec. k] | 3.8 |
| Sem push; alerta de criança mostra telefone de referência | Q19 [dec. k] | 3.4.1, 4.9 |
| Auditoria por 1 ano; perfil apagado 1 ano após o último acesso, exceto banidos | Q21 [dec. k] | 10.1.2 |
| Sem pesquisa por nome na rede; exportação só do próprio abrigo, sem `person_id` | Q22 [dec. k] | 4.11, 8.8 |
| `validator` e `civil_defense` por CLI | Q23 [dec. k] | 2.1 |
| Pessoa em abrigo não confirmado ou sem abrigo → fila central (`mediator` e DC) | Q24 + A4 [dec. k] | 4.8 |
| Busca ativa até o descarte; aviso a quem registrou antes de apagar | Q25 + A5 [dec. k] | 3.7 |
| Sessão de 7 dias, renovada a cada uso | Q26 [dec. k] | 3.1 |
| Selfie por turno fora da v1 | Q27 [dec. k] | 1.3 |
| `no_biometrics` como caminho de primeira classe | Q28 + C1 [dec. k] | 3.4, 4.1 |
| Sem backup do Firestore no piloto | Q32 [dec. k] | 6.3 |
| Fotos para token até 800 KB, não 300 KB | Q34 alterada [dec. k] | 6.1.4 |
| Base legal do credenciamento: art. 11, II, "g" | Q35 [dec. k] | 10.1.1 |
| Abrigo de evento encerrado não aceita escrita | Q36 [dec. k] | 4.3 |
| Removido só volta por QR presencial | Q37 [dec. k] | 3.3 |
| Código de aval presencial expira em 10 min | Q38 [dec. k] | 3.9 |
| Limiares de anomalia, só informativos no piloto | Q39 [dec. k] | 4.10 |
| Amostragem de liveness em 5% das entregas comuns | Q41 [dec. k] | 3.5.3 |
| Pessoa `assisted` recebe pelo cartão; selfie oferecida, não exigida | Q42 rejeitada [dec. k] | 3.4, 3.5.3, 4.1 |
| Seletor PT/EN/ES próprio na tela de consentimento | Q43 [dec. k] | 11.3 |
| Código do cartão de 8 caracteres; cartão do núcleo, válido na rede | Q44 + A3 [dec. k] | 3.5.1 |
| Busca offline pela fila, fotos cifradas e apagadas após o envio | Q45 [dec. k] | 3.7, 6.1.7 |
| "Preciso de resgate" vira 193/199 sem gravar alerta | C3 [dec. h] | 16.1 |
| Conferência no check-in; entrega por cartão; liveness em alto valor e por amostragem | A1 [dec. h] | 3.5 |
| Resgate no ponto de desembarque | A11 [dec. h] | 3.6 |
| EUA no piloto; região por instalação | A8 [dec. h] | 6, 10.1.4 |
| Consentimento em espanhol | M3 [dec. h] | 11.3 |
| Recusar a selfie não tira abrigo nem doação; taxa de recusa no painel | C1 [dec. k] | 3.4, 3.10 |
| Criança `with_guardian`/`separated`/`unaccompanied`; vínculo `declared`/`document_checked`; alerta; fila de proteção | C2 [dec. k] | 3.4.1, 3.8, 4.8, 4.9 |
| Núcleo familiar como entidade; cartão pertence ao núcleo; animais como contagem | A3 [dec. k] | 3.5.1, 7 |
| Papel `mediator`; admin é o mediador padrão; orientação para apagar o contato | A4 com ajuste do dono [dec. k] | 2.1, 4.8 |
| Falso positivo: conferência e decisão `not_the_person` | A6 [dec. k] | 4.8 |
| Painel no formato da DC: capacidade, vagas, recortes, saídas por destino, CSV diário, desabrigado x desalojado | A7 [dec. k] | 3.10 |
| Modo degradado | A12 [dec. k] | 5.7 |
| Admin assume pendências; troca de operador por PIN | M1 [dec. k] | 3.1.1, 9.2 |
| Nome social; vulnerabilidades estruturadas; consentimento em voz alta; frase sobre polícia e imigração | M2 [dec. k] | 3.4, 10.1.3 |
| Necessidades como lista fechada | M4 [dec. k] | 3.4, 7 |
| Destino na saída | M5 [dec. k] | 3.8, 3.10 |
| Histórico de outros abrigos só para admin | M6 [dec. k] | 4.11 |
| Contar justificativas de "outro local" para calibrar o raio | M7 [dec. k] | 3.2, 3.10 |
| Lacunas: capacidade, papel de proteção, desabrigado x desalojado, destino, contatos de referência por evento, animais | Seção 3 da revisão [dec. k] | 2.1, 3.10, 7 |
| CadÚnico, saúde e Cruz Vermelha/RFL fora da v1 | [dec. k] | 1.3 |
| Entram agora: Nominatim e busca offline | [dec. k] | 6.1.6, 3.7 |
| Para depois do piloto: aval remoto, fusões, fotos do local, "Estou Seguro" | [dec. k] | 16 |
| Adiadas com esse escopo: Q15, Q20, Q29, Q30, Q31, Q40 | [dec. k] | 16 |
| Implementação, logs, commits e PRs em inglês | [dec. j] | 0 |
| Entrega de alto valor ou sorteada sem selfie: admin confere pelo cartão e aprova na hora, com auditoria; sem fila de revisão | Q46, opção (c) [dec. l] | 3.5.3, 4.7 |
| Quem decide primeiro fecha o match (admin ou `mediator`); `mediator` vale para a rede toda | Q47 [dec. l] | 4.8 |
| `protection` concedido por DC ou validador, como o `mediator` | Q48 [dec. l] | 2.1 |
| Human Token funciona para crianças; miniatura para bebês e crianças sem identificação | Q49 [dec. l] | 3.4.1 |
| Entregas pseudonimizadas guardadas por 90 dias no piloto | Q50 [dec. l] | 6.2.2, 10.1.2 |
| Sem controlador, as telas mostram o responsável pelo piloto (`PILOT_RESPONSIBLE`) | Q51 [dec. l] | 10.1.3 |
| Miniatura de resgatado sem abrigo: voluntário do ponto de desembarque que o registrou e DC | Q33 [dec. l] | 4.6 |
| Design system "Acolhida", com fonte monoespaçada nos códigos e tema escuro; tokens em `design/` | [dec. l] | 6.1 |

### 13.2 Decisões técnicas
| Decisão | Motivo |
|---|---|
| Revalidação por comparação de IDs derivados | Mesmo resultado e custo do flow `+revalidation`, sem guardar o token puro. Rejeitado `humantoken+revalidation`: exigiria o token puro em `references`. |
| Lock global em memória para criação de abrigo | Uma instância; resolve a corrida dos 100 m. Rejeitado: transação com consulta geográfica (9 consultas) e documento-trava por célula. |
| Tarefa horária dentro do processo | Uma unidade systemd a menos. Rejeitado systemd timer/cron. |
| Idempotência genérica por `Idempotency-Key` | A fila só reenvia requisições HTTP; nenhuma chamada repetida (paga) à Unico. Rejeitado endpoint `/sync` em lote. |
| IDs gerados no cliente para entidades criadas offline | Referência entre itens da fila antes da sincronização e deduplicação natural. |
| Biometria tardia migra o registro para o ID derivado do token | Mantém "unicidade pelo ID do documento" [dec. g] e ainda permite cadastrar sem biometria (C1, A12). Rejeitado: ID próprio + índice de tokens, que mudaria a dec. g. |
| Modo degradado por motivo `limit` só sai por comando do operador | Toda chamada à Unico é paga; não há sonda gratuita. |
| Fotos da fila cifradas com chave WebCrypto não exportável | Uma regra para todas as fotos pendentes, não só a busca; sem senha extra para o voluntário. |
| PIN local com PBKDF2 | Troca de operador offline sem o servidor; o PIN nunca sai do aparelho. |
| Decisão de match em transação que exige `status = open` | Implementa "quem decide primeiro fecha" [dec. l, Q47] sem atribuição manual. |
| Dois eventos alcançados: entra no do abrigo mais próximo e alerta | "Detecção e aviso" do piloto sem fusão [dec. k]. |
| Sorteio de liveness determinístico pelo `delivery_id` | A retentativa da mesma entrega não muda o sorteio. |
| Código do cartão curto e digitável | Funciona com pulseira improvisada e sem câmera boa. |
| Busca responde igual com ou sem match | Até "não encontrado" é informação para um agressor. |
| Geohash com 9 células + haversine | Exigido pela dec. g; consultas de intervalo simples. |
| Preact + `idb` + `vite-plugin-pwa`; `BarcodeDetector` com `jsQR` de reserva | PWA pequena; Safari no iOS não tem `BarcodeDetector`. Rejeitado React (peso). |
| Mediação por mensagem `wa.me` | Sem serviço pago de SMS/e-mail. |
| Nominatim chamado pelo celular | Permitido pela dec. g; evita saída na VM. |
| Infra só pelos playbooks existentes | `provision.yml`/`configure.yml` já criam VM, Firestore e Caddy; o deploy só acrescenta tarefas. |

---

## 14. Estrutura de arquivos do repositório

```
api/                          Rust (cargo), binário `taba`
  Cargo.toml
  src/main.rs config.rs auth.rs unico.rs geo.rs shelters.rs events.rs members.rs
      people.rs households.rs deliveries.rs rescues.rs searches.rs
      dashboard.rs jobs.rs audit.rs idempotency.rs
  tests/                      integração com Unico falso e emulador do Firestore
pwa/                          Vite + TypeScript + Preact
  src/main.tsx routes.ts api.ts queue.ts db.ts crypto.ts camera.ts image.ts geo.ts nominatim.ts qr.ts
  src/screens/*.tsx
  src/i18n/pt-BR.json en.json es.json
infra/                        já existe
  README.md
  ansible/playbooks/provision.yml configure.yml   (configure ganha: serviço, swap, deploy)
  ansible/templates/Caddyfile.j2 env.j2           (Caddyfile ganha reverse_proxy; env.j2 ganha variáveis)
  ansible/templates/taba.service.j2               (novo)
  ansible/inventory/example/                      (no git)
  firestore/firestore.indexes.json                (novo)
design/                       tokens.json e tokens.css do design system (6.1)
.env.example                  todas as variáveis da 6.2.2, sem valores
docs/pt_br/spec/spec.md       este arquivo (versão em inglês: docs/en/spec/spec.md)
docs/pt_br/RIPD.md            antes de dados reais
```

Commits e pull requests em inglês [dec. j].

---

## 15. Critérios de aceite

Testes automatizados usam um servidor Unico falso (`UNICO_URL`) e o emulador do Firestore. Nenhum teste automatizado chama a Unico real.

**Unico e modo degradado**
1. `extract_token` devolve `None` para `value` ausente, `""` e `"  "`; nenhum desses cria pessoa ou voluntário.
2. `429` com "monthly process limit" não é repetido, grava `system/status` como `degraded`/`limit` e gera alerta `unico_limit`; `429` sem essa mensagem é repetido até 4 vezes e depois vira `503 unico_busy` com `Retry-After`.
3. Em modo `limit`, nenhuma chamada à Unico é feita até `taba unico resume`; rotas dependentes respondem `503 unico_degraded`.
4. Em modo degradado, check-in sem biometria, núcleo, cartão e entrega comum funcionam; entrega `high_value` usa a aprovação do admin ou, sem admin no aparelho, vai para `in_review` com `liveness_unavailable`.
5. Imagem > 800 KB → `413 image_too_large` sem chamar a Unico; o app produz fotos abaixo de 800 KB.
6. Captura só é consumida uma vez e só pelo `device_id` que a criou.
7. (Manual, primeira tarefa) Jornada aberta a partir da PWA instalada no iPhone volta pelo `callbackUri` e gera sessão.

**Abrigo e evento**
8. `accuracy` 101 m → `422 insufficient_accuracy`.
9. Segundo abrigo a 80 m de um `open` → `409 shelter_nearby` com primeiro nome do criador e sem telefone se `show_phone = false`; com `other_site_reason` → criado `flagged`, alerta `shelter_flagged` e contagem no painel.
10. Abrigo a 40 km de abrigo de evento ativo entra nele; a 60 km cria evento novo; a ≤ 50 km de abrigos de dois eventos entra no do abrigo mais próximo e gera `event_overlap`, sem fundir.
11. Evento sem escrita há 30 dias vira `closed`, com `purge_at`; miniaturas apagadas; buscas ativas sem match geram `search_expiring` para quem as registrou.
12. Duas criações simultâneas a 50 m: só uma é criada sem conflito.
13. Não existem rotas de fusão de abrigos, aval remoto, fotos do local nem "Estou Seguro" no piloto.

**Confirmação**
14. Aval presencial com celulares a 150 m → `422 too_far`; avalista membro ativo do abrigo novo → `403 cannot_endorse`.
15. Revogação volta o abrigo a `unconfirmed` e sinaliza os avalistas.
16. Abrigo `unconfirmed`: `POST /searches` → `403`; exportação → `403`; histórico do check-in sem `stays` de outros abrigos.

**Convites, membros e papéis**
17. Link com `role = admin` → `422 link_volunteer_only`.
18. QR usado duas vezes → segunda `410`; QR com 31 min → `410`.
19. Admin removendo admin → `403`; último admin saindo → `409 last_admin`.
20. Voluntário banido não cria sessão (`403 banned`) nem aceita convite.
21. Removido de um abrigo não volta por link (`403 removed_from_shelter`), mas volta por QR.
22. Só Val ou DC concedem `mediator`; voluntário comum recebe `403`.

**Pessoas, núcleos, cartões, entregas, resgates**
23. Mesmo token em dois check-ins → um documento em `people`, duas `stays`, uma aberta.
24. Check-in offline gera `assisted`; captura com liveness posterior → mesmo documento, `verified`.
25. Recusa da selfie gera `no_biometrics` com `biometrics_declined = true`; a pessoa entra em núcleo, recebe cartão e recebe entrega comum (`201`, `verification = card`).
26. `PUT /people/{uuid}/biometrics` com foto cria `people/{hmac}`, reaponta núcleo e entregas, apaga o registro antigo e guarda `former_ids`; se o `hmac` já existia, junta as `stays`.
27. Pessoa `assisted` recebe entrega comum pelo cartão sem `428`.
28. Entrega comum por cartão, não sorteada → `201` com `verification = card`, sem chamada à Unico.
29. Entrega de item `high_value` sem `capture_id` → `428 liveness_required`; com captura de outra pessoa → `409 revalidation_failed` e nada gravado; com `admin_approval: true` enviado por admin do abrigo → `201`, `status = ok`, `verification = admin_approval`, `approved_by` e auditoria com o nome do admin; enviado por voluntário → `403 admin_approval_required`.
30. Mesma entrega reenviada (mesmo `delivery_id`) tem o mesmo resultado de sorteio.
31. Item `per_household` entregue a dois membros do mesmo núcleo dentro do período → o segundo fica `in_review` com `duplicate_in_period`; item `per_person` não.
32. Item `once_per_event` entregue de novo no mesmo evento, mesmo depois de `period_days`, → `in_review`.
33. Entrega offline de item `high_value` sem sessão de admin no aparelho → `in_review`, motivo `liveness_unavailable`; com aprovação do admin → `ok`.
34. Pessoa em dois núcleos → `409 person_in_other_household`; segundo cartão ativo do núcleo → `409 household_has_card`.
35. `GET /cards/{code}` e `GET /households/{id}` não devolvem abrigo atual nem histórico.
36. Voluntário de abrigo `confirmed` vê só `has_other_stays`; admin vê as `stays`.
37. Saída de adulto sem `exit_destination` → `422`; o painel conta saídas por destino.
38. `needs` só aceita os quatro campos booleanos; texto livre → `422`.
39. Preencher `name` de pessoa com miniatura apaga `thumbnail_encrypted` na mesma escrita.
40. `GET /people/{id}/thumbnail` por voluntário de outro abrigo → `403`; acesso permitido grava auditoria.
41. 50 resgates do mesmo voluntário em 10 min não geram `anomaly_volume`; 50 check-ins geram, com `informational = true` e fora da caixa de "Avisos" do admin.

**Criança**
42. Check-in de criança `unaccompanied` gera `child_separated` para proteção, DC e admins, com os telefones de `reference_contacts`.
43. Saída de criança `separated` por admin → `403 protection_role_required`; por `protection`, com revalidação ok, → `200`.
44. Saída de criança `with_guardian` com vínculo `declared` por voluntário → `403 protection_role_required`; com `document_checked` e revalidação ok → `200`.
45. Revalidação falhou → `409`; saída por exceção por admin → `403`; por `protection` ou DC → `200` e alerta `exceptional_exit`.
46. Match de criança vai para `mediator_kind = protection` e não aparece para admins do abrigo.

**Busca e mediação**
47. `POST /searches` devolve corpo idêntico com e sem match; a 11ª busca do dia do mesmo voluntário → `429 search_limit`.
48. Busca enfileirada offline: a foto fica cifrada no IndexedDB (não legível como JPEG) e é apagada após o `2xx`.
49. Match de pessoa em abrigo `confirmed` aparece para os admins desse abrigo e para `mediator`; o contato da família só vem com `decision = accept`; segunda decisão → `409 match_already_decided`.
50. `not_the_person` encerra o match, mantém a busca ativa e não cria novo match para o mesmo `person_id`.
51. Check-in posterior de pessoa procurada cria match; repetir não cria segundo match.

**Painel**
52. O painel conta `assisted` e `no_biometrics`, mostra capacidade, lotação e vagas, recortes por faixa etária e vulnerabilidade, saídas por destino, taxa de recusa e o aviso sobre desalojados.
53. `daily.csv` tem uma linha por abrigo e não contém nome nem `person_id`.

**Offline e sincronização**
54. Com a rede desligada: criar abrigo, 3 check-ins por foto, 1 sem biometria, 1 núcleo com cartão, 2 entregas, 1 resgate e 1 busca; religar: tudo sincroniza em ordem, os `Blob`s são apagados e o contador volta a zero.
55. Reenviar o mesmo item da fila não gera segunda chamada à Unico.
56. Item com `401` não é reenviado com a sessão de outro voluntário; após "Assumir pendências" por um admin, é enviado e a auditoria registra `taken_over_from`.
57. Troca de operador por PIN funciona offline; cinco PINs errados apagam a sessão daquele voluntário no aparelho.

**Idiomas**
58. Celular em `pt-PT` abre em pt-BR; em `es` abre em inglês, exceto a tela de consentimento, que abre em espanhol; escolha manual persiste após recarregar e, logado, em outro aparelho.

**Segurança e implementação**
59. Nenhuma resposta da API contém o token Unico puro; nenhum documento do Firestore contém o token puro nem foto de pessoa (só miniatura cifrada).
60. Após o descarte, entregas não têm `person_id` nem `card_code`; pessoas, núcleos e buscas do evento não existem mais.
61. A chave da Unico, `HMAC_KEY` e `THUMBNAIL_KEY` não aparecem em nenhum arquivo do repositório nem da PWA compilada; o repositório não contém ID de projeto GCP nem IP.
62. Rotas, coleções, campos, enums, variáveis de ambiente e mensagens de log estão em inglês (revisão de código).

---

## 16. Fases posteriores ao piloto

Desenho mantido para depois do piloto [dec. k]. Nada desta seção é construído na v1; as questões adiadas com esse escopo ficam aqui.

### 16.1 "Estou Seguro" [dec. k; desenho: dec. h, C3, M3]
Construído depois do núcleo (abrigo, check-in, doação, busca).
1. Página pública `/safe`, sem login, em PT | EN | ES [dec. h, M3].
2. No topo, sempre visível: **"Precisa de resgate? Ligue 193 (Bombeiros) ou 199 (Defesa Civil)"**, com os botões `tel:193` e `tel:199`. Nada é gravado por esse caminho [dec. h, C3]. O alerta "preciso de resgate" só volta quando um centro de operações se comprometer a monitorar a fila.
3. A pessoa lê o aviso (finalidade, prazo, base legal `consent`) e faz captura com liveness (`purpose: "safe_check"`).
4. Escolhe "estou seguro" (`safe`) ou "fui para um abrigo" (`went_to_shelter`, com lista opcional de abrigos próximos). GPS opcional.
5. Opção de consentimento prévio: "Se alguém da minha família me procurar, pode saber que estou seguro (sem dizer onde estou)" (`family_contact_allowed`) [dec. k, Q24].
6. `POST /v1/safe-check {capture_id, status, reported_shelter_id?, lat?, lon?, family_contact_allowed, language}` cria ou atualiza `people/{person_id}` com `level = verified` e o bloco `safe_check: {status, reported_shelter_id?, lat?, lon?, family_contact_allowed, at}`; `origin = safe_check`.
7. Mediação: com `family_contact_allowed`, `mediator_kind = search_origin` (admins do abrigo que registrou a busca recebem só "declarou-se segura em <data>" e o contato da família); sem ele, match gravado `declined_in_advance`, sem aviso.
8. Registro sem evento: apagado 30 dias após o registro.
9. **Q15 (adiada):** limites para capturas públicas (por IP ou por aparelho, e total por dia). A revisão recomenda limite por aparelho com captcha, não por IP.
10. Critérios de aceite: `/safe` mostra `tel:193` e `tel:199` e não existe rota nem coleção que grave pedido de resgate; sem consentimento prévio, match `declined_in_advance` e nenhum aviso.

### 16.2 Aval remoto e cadeia de confirmação [dec. k]
No piloto: validador ou aval presencial.
1. Admins de abrigos com `can_endorse`, do mesmo evento, veem `GET /v1/events/{id}/shelters-to-endorse` (fotos do local, endereço, nome do criador, distância).
2. `POST /v1/shelters/{id}/endorsements/remote {endorser_shelter_id}`.
3. Com dois avais de dois abrigos diferentes, por dois voluntários diferentes, nenhum ligado ao abrigo novo: `confirmed`, `confirmation_type = remote`, `can_endorse = false` até receber aval presencial ou de validador.
4. **Q30 (adiada):** "ligado ao abrigo novo" = ter ou ter tido `membership` com ele.
5. **Q20 (adiada):** revogação não afeta os abrigos que o revogado avalizou; só sinaliza quem deu aval a ele.

### 16.3 Fusão de abrigos e de eventos [dec. i, Q2; dec. k]
No piloto: só detecção e aviso; a DC resolve à mão (3.2.1, 4.3).
- **Fusão de abrigos (pedido):** no conflito offline, "Fundir meus cadastros no abrigo X" cria a origem como `unconfirmed` e um pedido em `merges/{merge_id}` (`source_shelter_id`, `target_shelter_id`, `requested_by`, `status: pending|accepted|declined`, `decided_by?`, `created_at`, `decided_at?`). Rotas `POST /v1/shelters/{id}/merges {target_shelter_id}` e `POST /v1/merges/{id}/decision {decision: accept|decline}` (admin do destino, abrigo `confirmed`). Na aceitação, pessoas, núcleos, cartões, entregas e resgates vão para o destino (o token resolve as repetidas); se já havia `stay` aberta no destino, mantém a de `entered_at` mais antiga; a origem fica `status = merged`, `merged_into = destino`; itens da fila que chegarem depois para a origem são aplicados ao destino. Antes de aceitar, o destino não vê nada da origem.
- **Q40 (adiada):** na fusão aceita, admins da origem viram voluntários no destino (`via = merge`).
- **Fusão de eventos:** quando um abrigo alcança dois ou mais eventos, sobrevive o mais antigo; os outros ficam `status = merged`, `merged_into`; abrigos, buscas, resgates e `people.event_ids` são reapontados em lotes, sob o mesmo lock global da criação de abrigo.
- **Abrigo distante:** na criação, se houver eventos ativos na região ou no estado e nenhum a ≤ R km, o app pergunta "Este abrigo recebe pessoas de algum destes eventos?" (`GET /v1/events/nearby`, `declared_event_ids`, `event_link = declared`) [dec. c].
- **Q31 (adiada):** o abrigo declarado participa da regra de cadeia como qualquer outro; "região ou estado" = mesma UF ou ≤ 300 km.

### 16.4 Fotos do local [dec. k]
- Até 2 fotos do local (sem pessoas), reduzidas a ≤ 200 KB (maior lado 1024 px), na subcoleção `shelters/{id}/photos/{n}` (`image: bytes`, `content_type`, `uploaded_by`, `uploaded_at`), usadas no aval remoto.
- Rotas `POST /v1/shelters/{id}/photos {image_base64}` e `GET /v1/shelters/{id}/photos/{n}` (admin do abrigo, avalista elegível, Val, DC).
- **Q29 (adiada):** quantidade, tamanho e guarda no próprio Firestore.
