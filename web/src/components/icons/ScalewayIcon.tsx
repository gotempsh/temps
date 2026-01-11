import React from 'react'

interface ScalewayIconProps {
  className?: string
  width?: number
  height?: number
}

export const ScalewayIcon: React.FC<ScalewayIconProps> = ({
  className = '',
  width = 24,
  height = 24,
}) => (
  <svg
    className={className}
    width={width}
    height={height}
    viewBox="0 0 24 24"
    fill="currentColor"
    xmlns="http://www.w3.org/2000/svg"
  >
    <path d="M15.748 3.997v4.125h4.125V3.997zM4.127 20.003h4.125v-4.125H4.127zm3.872-4.378v-3.872H4.127v3.872zm.253-4.125h3.872V7.628H8.252zm4.125 0h3.872V7.628h-3.872zm4.125 0h3.372v3.872h-3.372zm-4.125 4.125H8.505v3.872h3.872zm4.125 0h-3.872v3.872h3.872z" />
  </svg>
)
