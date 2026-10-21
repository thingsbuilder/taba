import { useState } from 'preact/hooks'
import type { ProfileInput } from '../api'
import { getLanguage, t } from '../i18n'

export const MAX_NAME = 100
export const MAX_PHONE = 30

/** Client-side mirror of the server rules, so the common mistakes never leave the phone. */
export function profileError(name: string, phone: string): string | null {
  if (name.trim() === '' || name.trim().length > MAX_NAME) return t('error.invalid_profile')
  if (phone.trim().length > MAX_PHONE) return t('error.invalid_profile')
  return null
}

/** Name, phone, "show my phone" and (for the first login) the language. */
export function ProfileForm(props: {
  initial?: { name: string; phone: string | null; show_phone: boolean }
  submitLabel: string
  askLanguage?: boolean
  onSubmit: (profile: ProfileInput) => void
}) {
  const [name, setName] = useState(props.initial?.name ?? '')
  const [phone, setPhone] = useState(props.initial?.phone ?? '')
  const [showPhone, setShowPhone] = useState(props.initial?.show_phone ?? false)
  const [language, setLanguage] = useState<'pt-BR' | 'en'>(getLanguage())
  const [error, setError] = useState<string | null>(null)

  function submit(e: Event) {
    e.preventDefault()
    const problem = profileError(name, phone)
    if (problem) return setError(problem)
    props.onSubmit({
      name: name.trim(),
      ...(phone.trim() ? { phone: phone.trim() } : {}),
      show_phone: showPhone,
      language,
    })
  }

  return (
    <form class="form" onSubmit={submit}>
      <label class="field">
        {t('profile.name')}
        <input type="text" autocomplete="name" maxLength={MAX_NAME} value={name} onInput={(e) => setName(e.currentTarget.value)} />
      </label>
      <label class="field">
        {t('profile.phone')}
        <input type="tel" autocomplete="tel" maxLength={MAX_PHONE} value={phone} onInput={(e) => setPhone(e.currentTarget.value)} />
      </label>
      <label class="check">
        <input type="checkbox" checked={showPhone} onChange={(e) => setShowPhone(e.currentTarget.checked)} />
        {t('profile.show_phone')}
      </label>
      {props.askLanguage && (
        <label class="field">
          {t('profile.language')}
          <select value={language} onChange={(e) => setLanguage(e.currentTarget.value as 'pt-BR' | 'en')}>
            <option value="pt-BR">Português</option>
            <option value="en">English</option>
          </select>
        </label>
      )}
      {error && <p class="notice notice-error" role="alert">{error}</p>}
      <button type="submit" class="button">{props.submitLabel}</button>
    </form>
  )
}
