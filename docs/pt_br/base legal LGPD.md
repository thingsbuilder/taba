# Base legal LGPD — app de emergência com Human Token (Unico)

> Leitura técnica da lei, não é parecer jurídico. Antes de usar com dados reais, valide com um advogado ou com o DPO de quem for operar o app.

## Biometria é dado sensível

Biometria facial é **dado pessoal sensível** (LGPD, art. 5º, II). Pelo art. 11, dado sensível só pode ser tratado com consentimento (inciso I) ou, sem consentimento, quando for indispensável para uma das hipóteses do inciso II.

## Art. 11, II, "e": proteção da vida

> "proteção da vida ou da incolumidade física do titular ou de terceiro"

### Por que se aplica à busca de desaparecidos

- O **titular** é o desaparecido. A família envia a foto dele, e ele não tem como consentir, porque está desaparecido.
- A **finalidade** é localizá-lo, ou seja, proteger a vida e a integridade física dele.
- O tratamento é **indispensável**: sem comparar rostos, não há como achar alguém que chegou ao abrigo sem documento ou que não consegue informar o próprio nome.

### O que essa base não autoriza

- **Outra finalidade.** Os tokens não podem ser usados depois para outros fins, como polícia, marketing ou cruzamento com outras bases (princípio da finalidade, art. 6º, I).
- **Guarda indefinida.** Passada a emergência, a justificativa acaba. O sistema precisa de uma regra de descarte, por exemplo apagar tudo X dias depois de a pessoa ser localizada ou de a operação terminar (arts. 15 e 16).
- **Coleta além do necessário.** Guarde o token, não a foto (princípio da necessidade, art. 6º, III). A própria Unico prevê descartar a selfie depois de gerar o token.
- **Uso quando o consentimento é viável.** No check-in de um adulto consciente no abrigo, peça consentimento. A base "proteção da vida" é mais sólida quando o titular não tem como consentir, que é como o GDPR trata o caso equivalente ("interesses vitais", art. 9(2)(c)). A LGPD não traz essa restrição de forma expressa, mas a ANPD tende a olhar assim.

## Base legal por tela do app

| Tela | Base mais adequada |
|---|---|
| Busca de desaparecido (foto enviada pela família) | Art. 11, II, "e": proteção da vida |
| Check-in no abrigo | Consentimento (art. 11, I) ou "e", se a pessoa estiver ferida ou incapacitada |
| Entrega de doação e revalidação | Art. 11, II, "g": prevenção à fraude e segurança do titular em identificação e autenticação |
| Operação da Defesa Civil ou prefeitura | Art. 11, II, "b": execução de políticas públicas pela administração pública |
| Prontuário de saúde | Art. 11, II, "f": tutela da saúde por profissional de saúde |

## Crianças (art. 14)

O tratamento precisa atender ao **melhor interesse da criança**. O § 3º permite coletar dados de criança sem consentimento dos pais quando isso for necessário para **contatar os responsáveis** ou para a **proteção da criança**, que é exatamente o caso de criança desacompanhada num abrigo.

## O que levar para a demo

- **Aviso curto** dizendo para que serve a coleta, por quanto tempo os dados ficam guardados e quem os acessa (transparência, art. 9º).
- **Registro da base legal** usada em cada operação.
- **RIPD** (Relatório de Impacto à Proteção de Dados, art. 38). Para dado biométrico em larga escala, a ANPD espera esse documento.
- **Controlador definido**, provavelmente a Defesa Civil ou a prefeitura, com a sua empresa como operadora. Isso muda quem responde pelos dados.
