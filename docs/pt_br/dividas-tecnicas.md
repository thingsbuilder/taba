# Dívidas técnicas

Cada limitação conhecida que o código aceita de propósito vira uma tarefa aqui, com o **limite mensurável**, o **comportamento ao ultrapassá-lo** e o **teste que o cobre**. Um comentário `ponytail` no código não substitui esta lista: quem aceita um limite escreve a tarefa.

Duas classes:

- **Perda de correção**: ao passar do limite o número fica errado. Só é aceitável se a resposta **diz** que ficou parcial; nunca apresentar subconjunto como total.
- **Conveniência**: ao passar do limite o sistema fica mais lento ou mais caro, mas o resultado continua correto.

## Perda de correção (a resposta marca como parcial)

### DT-02. Saídas e movimentação do painel: varredura de 10.000 pessoas por evento
- **Onde:** `api/src/dashboard.rs`, `MOVES_SCAN_LIMIT`, `scan_moves`.
- **Limite:** 10.000 pessoas com o evento em `event_ids` (lê-se 10.001 para saber se passou).
- **Ao ultrapassar:** o painel devolve `complete: false` e `partial_indicators` com `exits`; a tela mostra o aviso de parcial. Os valores `movement` e `exits_by_destination` contam só as pessoas lidas.
- **Teste:** `the_moves_scan_is_marked_partial_only_past_its_limit` (limite−1, limite, limite+1).
- **Saída da dívida:** guardar os contadores no documento do evento, escritos pelas rotas de saída e de check-in.

### DT-03. Animais do painel: varredura de 2.000 núcleos por abrigo
- **Onde:** `api/src/dashboard.rs`, `PETS_SCAN_LIMIT`, `pets_of`.
- **Limite:** 2.000 núcleos criados no abrigo.
- **Ao ultrapassar:** `partial_indicators` com `pets`; `pets` do abrigo e do total contam só os núcleos lidos.
- **Teste:** `the_animals_scan_is_marked_partial_only_past_its_limit`.
- **Saída da dívida:** mesma do DT-02 (contador no abrigo).

### DT-04. Painel: 500 abrigos por evento, 100 eventos na lista
- **Onde:** `api/src/dashboard.rs`, `SHELTER_LIMIT`, `EVENT_LIMIT`, `shelters_of`, `build_list`.
- **Limite:** 500 abrigos por evento; 100 eventos na lista.
- **Ao ultrapassar:** o detalhe devolve `complete: false` com `shelters` (os totais de pessoas somam só os abrigos lidos); a lista devolve `complete: false`.
- **Teste:** `an_indicator_cut_at_its_limit_is_marked_partial_in_the_answer`, `the_list_is_marked_partial_only_past_the_event_or_shelter_limit`; o corte em si em `a_read_is_cut_only_when_it_returns_more_than_the_limit`.
- **Saída da dívida:** paginar abrigos e eventos, ou agregar por evento no documento do evento.

### DT-05. Exportação do abrigo: 5.000 linhas por seção
- **Onde:** `api/src/export.rs`, `MAX_ROWS`, `build`.
- **Limite:** 5.000 linhas em cada seção (`people`, `deliveries`); a seção `households` depende das pessoas lidas.
- **Ao ultrapassar:** a seção é cortada e o CSV traz a linha `# warning,truncated,<seção>,<limite>`.
- **Teste:** `the_export_has_every_eligible_row_and_says_when_it_cut` (limite−1, limite, sem corte).
- **Saída da dívida:** exportar em páginas (cursor) ou por trabalho em segundo plano.

## Conveniência

### DT-01. Cache do painel por processo
- **Onde:** `api/src/dashboard.rs`, `TtlCache`.
- **Limite:** uma instância da API. Com duas ou mais, cada uma mantém o próprio cache e reconstrói o painel por conta própria.
- **Ao ultrapassar:** mais leituras no Firestore (uma reconstrução por instância e por chave), sem número errado: o painel mostra `generated_at` de cada resposta. Dentro de uma instância há uma reconstrução por chave por vez, no máximo 256 chaves por cache e 16 consultas `count()` simultâneas no processo.
- **Teste:** `the_cache_runs_one_build_per_key_at_a_time`, `the_cache_keeps_at_most_its_limit_of_keys`. O comportamento com várias instâncias não tem teste (exige duas instâncias).
- **Saída da dívida:** cache compartilhado.

### DT-06. Lista de abrigos a confirmar: uma leitura por abrigo
- **Onde:** `api/src/dashboard.rs`, `to_confirm`.
- **Limite:** uma leitura do voluntário que criou, em série, por abrigo não confirmado (no máximo 500 por evento).
- **Ao ultrapassar:** a resposta demora mais; o resultado é o mesmo (e fica no cache).
- **Teste:** `the_list_has_the_summary_and_the_shelters_to_confirm` cobre o resultado; não há teste de tempo.

### DT-07. Estatísticas do evento no descarte: uma contagem por abrigo
- **Onde:** `api/src/export.rs`, `event_stats`.
- **Limite:** uma contagem de núcleos por abrigo do evento, em série.
- **Ao ultrapassar:** o descarte do evento demora mais; o resultado é o mesmo.
- **Teste:** `event_stats_count_the_event_without_personal_data`.

### DT-08. Alertas pessoais dividem a cota de 200 com anomalias sobre o voluntário
- **Onde:** `api/src/alerts.rs`, `open_from`, fonte `Personal`.
- **Limite:** 200 alertas abertos por fonte; a fonte pessoal traz tudo que cita o voluntário em `volunteer_ids`, inclusive anomalias e `endorser_flagged` sobre ele, que `is_recipient` descarta depois.
- **Ao ultrapassar:** com mais de 200 desses, mais novos que os `search_expiring` e `family_search_received`, os alertas pessoais deixam de aparecer. Perda de correção, mas rara (exige centenas de anomalias abertas sobre uma só pessoa).
- **Correção prevista:** filtrar a fonte por `kind IN (search_expiring, family_search_received)` e criar o índice `(resolved, volunteer_ids CONTAINS, kind, created_at DESC)`.
- **Teste:** a criar junto da correção.

## Comentários `ponytail` de outros arquivos

Ainda sem tarefa aqui; cada dono converte os seus em limite mensurável, comportamento ao ultrapassar e teste: `purge.rs`, `idempotency.rs`, `events.rs`, `shelters.rs`, `audit.rs`, `unico.rs`, `alerts.rs`, `matches.rs`, `search_links.rs`, `jobs.rs`, `people.rs`.
