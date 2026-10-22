// /sobre: language toggle and scroll-in. Page works without it (Portuguese, everything visible).
(function () {
  var root = document.documentElement
  var saved = null
  try { saved = localStorage.getItem('sobre-lang') } catch (e) { /* private mode */ }
  function setLang(l) {
    root.lang = l === 'en' ? 'en' : 'pt-BR'
    document.querySelectorAll('[data-lang]').forEach(function (b) {
      b.setAttribute('aria-pressed', String(b.dataset.lang === l))
    })
    try { localStorage.setItem('sobre-lang', l) } catch (e) { /* ignore */ }
  }
  setLang(saved || (/^en/i.test(navigator.language) ? 'en' : 'pt'))
  document.querySelectorAll('[data-lang]').forEach(function (b) {
    b.addEventListener('click', function () { setLang(b.dataset.lang) })
  })

  if (!('IntersectionObserver' in window)) return
  root.classList.add('js')
  var io = new IntersectionObserver(function (es) {
    es.forEach(function (e) { if (e.isIntersecting) { e.target.classList.add('in'); io.unobserve(e.target) } })
  }, { threshold: 0.15 })
  document.querySelectorAll('.reveal').forEach(function (el) { io.observe(el) })
})()
