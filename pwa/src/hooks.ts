import { useEffect, useState } from 'preact/hooks'
import { getLanguage, onLanguageChange } from './i18n'

export function useLanguage() {
  const [language, setLanguage] = useState(getLanguage())
  useEffect(() => onLanguageChange(() => setLanguage(getLanguage())), [])
  return language
}

export function useOnline() {
  const [online, setOnline] = useState(navigator.onLine)
  useEffect(() => {
    const update = () => setOnline(navigator.onLine)
    window.addEventListener('online', update)
    window.addEventListener('offline', update)
    return () => {
      window.removeEventListener('online', update)
      window.removeEventListener('offline', update)
    }
  }, [])
  return online
}
