# Taba

O Taba é uma rede open source de abrigos para desastres no Brasil. Este design system serve a aplicação web instalável (PWA) usada por voluntários em abrigos, no ponto de desembarque e na busca de desaparecidos.

A direção visual se chama **Acolhida**: macia e calma, porque do outro lado da tela está alguém que acabou de perder a casa. Ela tem duas exceções deliberadas: **códigos em fonte monoespaçada**, para ditar sem erro, e **tema escuro**, para plantões noturnos e economia de bateria.

## Fundamentos de conteúdo

- **Português simples, frases curtas.** Escreva como um voluntário falaria: "Check-in com selfie", não "Iniciar captura biométrica".
- **Nunca culpe a pessoa.** "Não conseguimos ler o rosto. Tente com mais luz", não "Foto inválida".
- **Recusar é sempre uma opção visível.** "Cadastrar sem selfie" tem o mesmo tamanho e a mesma posição de destaque que o caminho com selfie. Recusar a selfie não tira abrigo nem doação.
- **Diga o estado da rede sem drama.** "Sem sinal · 4 na fila" informa; "Erro de conexão!" assusta.
- **Nomes de quem não tem nome:** use o número da pulseira ("Pulseira 0217"), nunca "Desconhecido".
- **Datas e horas relativas** em listas ("há 2 min"); absolutas em registros e exportações.

## Fundamentos visuais

### Cor
- O fundo é `bg`; cartões e barras ficam em `surface`, separados por `shadow-card`, não por borda.
- `primary` é reservada para a ação principal da tela e para o item ativo. Uma ação principal por tela.
- A ação secundária usa `primary-soft` com `on-primary-soft`.
- `urucum` é o acento da marca. Use em formas e no logotipo, nunca em texto pequeno.
- Estados têm sempre um fundo suave e um texto escuro do mesmo tom (`warning-soft` + `on-warning`, e assim por diante). Nunca diferencie estados só pela cor: o chip sempre tem texto.

| Nível da pessoa | Fundo | Texto | Rótulo |
|---|---|---|---|
| `verified` | `success-soft` | `on-success` | Verificado |
| `assisted` | `warning-soft` | `on-warning` | Assistido |
| `no_biometrics` | `neutral-soft` | `on-neutral` | Sem biometria |

- **Proibido:** vermelho com branco em forma de cruz. A cruz vermelha é emblema protegido por lei.

### Tipografia
- **Outfit** (`display`) em títulos; é a mesma família de formas do logotipo.
- **Figtree** (`body`) em todo o resto. Texto base de 16 px; nada abaixo de 13 px.
- **IBM Plex Mono** (`mono`) só em códigos de cartão e pulseira: `K7Q4-M2XR`.
- As três vêm do Google Fonts.

### Espaço, forma e toque
- Margem lateral da tela: `space-4` (20 px). Entre blocos: `space-3`.
- Botões e cartões principais com `radius-lg` (18 px); chips com `radius-pill`.
- Botões com `control-height` (52 px). Nenhum alvo de toque menor que `touch-min` (48 px): o app é usado em pé, com pressa, às vezes com a mão molhada.

### Elevação
Uma só: `shadow-card`. Nada de gradientes.

## Componentes recorrentes (descrição)

- **Barra do abrigo:** logotipo, nome do abrigo (`title`), município e estado de confirmação.
- **Aviso de rede:** chip `warning-soft` com o ícone de sem sinal e a contagem da fila. Fica visível sempre que houver pendência.
- **Lotação:** cartão com "142 de 200" e barra `primary` sobre `surface-sunken`.
- **Lista de cadastros:** cartão por pessoa com avatar (iniciais ou `#` para pulseira), nome em `body-strong`, horário em `caption` e chip do nível.
- **Navegação inferior:** quatro itens (Início, Check-in, Doações, Busca); o ativo com `primary-soft`.

## Iconografia

Traço de 2 px, pontas arredondadas, sem preenchimento, na cor do texto ao lado. Sem emoji. Ícone sozinho só com rótulo acessível (`aria-label`).

## Acessibilidade

- Contraste mínimo de 4,5:1 para texto nos dois temas; os pares de tokens acima já cumprem.
- Anel de foco `focus`, 2 px, sempre visível no teclado.
- O tema segue o do aparelho; o escuro é completo, não uma inversão.

## Logotipo

A marca é um anel de seis ocas em volta de um centro em `urucum`: a disposição de uma taba, a palavra tupi para aldeia. Use `taba-mark.svg` em fundo claro, `taba-mark-dark.svg` em fundo escuro e `taba-app-icon.svg` como ícone do app.
